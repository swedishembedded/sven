// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`AgentBuilder`] — single entry point for constructing a fully wired Agent.
//!
//! Callers pass a [`Config`], an optional [`RuntimeContext`], the desired
//! mode and model, and a [`ToolSetProfile`].  The builder handles registry
//! construction and [`AgentRuntimeContext`] population internally.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, Mutex};
use tokio::time::Instant;
use tracing::{info, warn};

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentNewParams, ModelResolver};
use sven_mcp_client::{McpManager, McpTool};
use sven_model::ModelProvider;
use sven_tools::{events::ToolEvent, PermissionRequester, SharedToolDisplays, SharedTools};

use crate::context::{RuntimeContext, ToolSetProfile};
use crate::registry::build_tool_registry;

/// Constructs a fully wired [`Agent`] from configuration and runtime context.
///
/// # Example
/// ```rust,ignore
/// let agent = AgentBuilder::new(config)
///     .with_runtime_context(RuntimeContext::auto_detect())
///     .build(mode, model, ToolSetProfile::Full { ... });
/// ```
pub struct AgentBuilder {
    config: Arc<Config>,
    runtime_ctx: RuntimeContext,
    /// Optional shared tool snapshot populated after registry construction so
    /// that the TUI can inspect available tools via `/tools`.
    shared_tools: Option<SharedTools>,
    /// Optional slot for the tool display registry; set after registry build
    /// so the TUI can render tool call/result summaries with ToolDisplay.
    shared_tool_displays: Option<SharedToolDisplays>,
    /// Optional IDE-backed permission requester.  When set, tools with
    /// `ApprovalPolicy::Ask` gate execution on an explicit IDE approval.
    permission_requester: Option<Arc<dyn PermissionRequester>>,
    /// When false (headless/CI), MCP OAuth flows are never triggered.
    allow_interactive_oauth: bool,
    /// When Some(ms), wait up to that many milliseconds for MCP tools to become
    /// available before building the registry. Used in headless mode so the
    /// conversation session gets tools from connecting MCP servers.
    wait_for_mcp_tools_ms: Option<u64>,
}

impl AgentBuilder {
    /// Create a builder with the given configuration.
    /// Runtime context defaults to empty (no project/git/CI detection).
    /// OAuth flows are allowed by default (interactive TUI).
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            runtime_ctx: RuntimeContext::empty(),
            shared_tools: None,
            shared_tool_displays: None,
            permission_requester: None,
            allow_interactive_oauth: true,
            wait_for_mcp_tools_ms: None,
        }
    }

    /// Disable interactive OAuth flows for headless/CI/batch runs.
    /// When false, MCP servers requiring OAuth will stay in NeedsAuth state
    /// instead of opening a browser. Pre-authenticate in an interactive
    /// session before running batch.
    pub fn with_allow_interactive_oauth(mut self, allow: bool) -> Self {
        self.allow_interactive_oauth = allow;
        self
    }

    /// Set the runtime context (project root, git, CI environment).
    pub fn with_runtime_context(mut self, ctx: RuntimeContext) -> Self {
        self.runtime_ctx = ctx;
        self
    }

    /// Inject a [`SharedTools`] handle that will be populated after the tool
    /// registry is built inside [`AgentBuilder::build`].
    ///
    /// The TUI holds a clone of this handle and calls `.get()` to obtain a
    /// cheap `Arc<[ToolSchema]>` snapshot when the `/tools` inspector is opened.
    pub fn with_shared_tools(mut self, shared_tools: SharedTools) -> Self {
        self.shared_tools = Some(shared_tools);
        self
    }

    /// Inject a slot that will be filled with the tool display registry after
    /// the tool registry is built. The TUI holds a clone and reads it for
    /// chat rendering (collapsed tool summary, display name).
    pub fn with_shared_tool_displays(mut self, slot: SharedToolDisplays) -> Self {
        self.shared_tool_displays = Some(slot);
        self
    }

    /// Wire up an IDE-backed permission requester.
    ///
    /// Tools with [`sven_tools::ApprovalPolicy::Ask`] will call
    /// `requester.request_permission()` before executing, blocking until the
    /// IDE approves or denies the call.
    pub fn with_permission_requester(mut self, requester: Arc<dyn PermissionRequester>) -> Self {
        self.permission_requester = Some(requester);
        self
    }

    /// In headless/CI mode, wait up to `timeout_ms` milliseconds for MCP
    /// servers to connect and expose tools before building the agent.
    /// Ensures the conversation session receives MCP tools rather than
    /// starting with none. Pass 0 to disable waiting (default).
    pub fn with_wait_for_mcp_tools(mut self, timeout_ms: u64) -> Self {
        self.wait_for_mcp_tools_ms = if timeout_ms > 0 {
            Some(timeout_ms)
        } else {
            None
        };
        self
    }

    /// Build the [`Agent`] with the given mode, model, and tool-set profile.
    ///
    /// This method owns the creation of the shared mode lock and tool-event
    /// channel so that `SwitchModeTool` / `TodoTool` and the agent loop
    /// operate on **the same** instances.
    pub async fn build(
        self,
        mode: AgentMode,
        model: Arc<dyn ModelProvider>,
        profile: ToolSetProfile,
    ) -> Agent {
        let (agent, _mcp, _rx) = self.build_with_mcp(mode, model, profile).await;
        agent
    }

    /// Like [`build`] but also returns the [`McpManager`] and the MCP event
    /// receiver so callers (e.g. the TUI) can react to auth/connection events.
    ///
    /// 1. Creates `mode_lock` (same Arc for both the registry and the Agent).
    /// 2. Creates `(tool_event_tx, tool_event_rx)` (tx → tools, rx → Agent).
    /// 3. Converts [`RuntimeContext`] → [`AgentRuntimeContext`].
    /// 4. Builds a [`ToolRegistry`] via `build_tool_registry`.
    /// 5. Probes the provider for the actual context window (`GET /props`).
    /// 6. Constructs `Agent::new(...)`.
    pub async fn build_with_mcp(
        self,
        mode: AgentMode,
        model: Arc<dyn ModelProvider>,
        profile: ToolSetProfile,
    ) -> (
        Agent,
        Arc<McpManager>,
        mpsc::Receiver<sven_mcp_client::McpEvent>,
    ) {
        // Shared mode lock: SwitchModeTool holds a clone; the agent owns it.
        let mode_lock = Arc::new(Mutex::new(mode));
        // Shared event channel: tools send, agent drains.
        let (tool_event_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);

        // Convert RuntimeContext → AgentRuntimeContext (the sven-core type).
        let mut runtime = self.runtime_ctx.to_agent_runtime();
        // Preserve any append/override fields that may have been set on the
        // RuntimeContext before it was passed to the builder.
        runtime.append_system_prompt = self.runtime_ctx.append_system_prompt;
        runtime.system_prompt_override = self.runtime_ctx.system_prompt_override;

        let (mcp_event_tx, mcp_event_rx) = mpsc::channel::<sven_mcp_client::McpEvent>(64);
        let mcp_manager = McpManager::new(
            self.config.mcp_servers.clone(),
            mcp_event_tx,
            self.allow_interactive_oauth,
        );
        mcp_manager.connect_all().await;
        mcp_manager.start_background_tasks();

        // In headless mode, wait for MCP tools so the conversation session
        // receives them rather than starting with none.
        let has_enabled_servers = self.config.mcp_servers.values().any(|c| c.enabled);
        if let Some(timeout_ms) = self.wait_for_mcp_tools_ms {
            if has_enabled_servers {
                let deadline = Instant::now() + Duration::from_millis(timeout_ms);
                let poll_interval = Duration::from_millis(200);
                loop {
                    let tools = mcp_manager.tools().await;
                    if !tools.is_empty() {
                        info!(
                            count = tools.len(),
                            "MCP tools available, proceeding with agent build"
                        );
                        break;
                    }
                    if Instant::now() >= deadline {
                        warn!(
                            timeout_ms,
                            "MCP tools not available within timeout, proceeding without"
                        );
                        break;
                    }
                    tokio::time::sleep(poll_interval).await;
                }
            }
        }

        let mut registry = build_tool_registry(
            &self.config,
            model.clone(),
            profile,
            mode_lock.clone(),
            tool_event_tx,
            runtime.clone(),
        );

        // Register MCP tools after core tools so that the Anthropic provider
        // can place BP1 after core tools and BP2 after MCP tools.
        let mcp_tools: Vec<McpTool> = mcp_manager.tools().await;
        if !mcp_tools.is_empty() {
            for tool in mcp_tools {
                registry.register(tool);
            }
        } else if !self.config.mcp_servers.is_empty() {
            warn!("No MCP tools available yet (servers may still be connecting)");
        }

        if let Some(req) = self.permission_requester {
            registry.set_permission_requester(req);
        }

        // Populate the shared tool snapshot so the TUI `/tools` inspector can
        // display all registered tools without accessing the registry directly.
        if let Some(ref st) = self.shared_tools {
            st.set(registry.schemas());
        }
        if let Some(ref slot) = self.shared_tool_displays {
            slot.set(registry.display_registry());
        }

        // Resolve context window: prefer live probe (actual n_ctx loaded by the
        // server), fall back to the static catalog, then default to 128 000.
        // The probe is a cheap GET /props request; it silently returns None for
        // hosted providers that don't expose such an endpoint.
        let context_window = match model.probe_context_window().await {
            Some(n) if n > 0 => n as usize,
            _ => model
                .config_context_window()
                .or_else(|| model.catalog_context_window())
                .unwrap_or(128_000) as usize,
        };

        // Build a resolver closure so the agent can switch models mid-turn
        // when the `switch_model` tool fires.  The closure captures the full
        // config and resolves the fuzzy model string to a live provider.
        let resolver_config = Arc::clone(&self.config);
        let model_resolver: ModelResolver = Arc::new(move |model_str: &str| {
            let model_cfg = sven_model::resolve_model_from_config(&resolver_config, model_str);
            let provider = sven_model::from_config(&model_cfg)?;
            Ok(Arc::from(provider) as Arc<dyn sven_model::ModelProvider>)
        });

        let agent = Agent::new_with_params(AgentNewParams {
            model,
            tools: Arc::new(registry),
            config: Arc::new(self.config.agent.clone()),
            runtime,
            mode_lock,
            tool_event_rx,
            max_context_tokens: context_window,
            model_resolver: Some(model_resolver),
        });

        (agent, mcp_manager, mcp_event_rx)
    }
}
