// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`AgentBuilder`] — single entry point for constructing a fully wired Agent.
//!
//! Callers pass a [`Config`], an optional [`RuntimeContext`], the desired
//! mode and model, and a [`ToolSetProfile`].  The builder handles registry
//! construction and [`AgentRuntimeContext`] population internally.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_core::Agent;
use sven_model::ModelProvider;
use sven_tools::{events::ToolEvent, OutputBufferStore, SharedTools};

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
    /// Shared buffer store for streaming subagent output.  Created by the
    /// builder and exposed via [`AgentBuilder::buffer_store`] so that callers
    /// (e.g. the TUI) can hold a reference for live rendering.
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    /// Optional shared tool snapshot populated after registry construction so
    /// that the TUI can inspect available tools via `/tools`.
    shared_tools: Option<SharedTools>,
}

impl AgentBuilder {
    /// Create a builder with the given configuration.
    /// Runtime context defaults to empty (no project/git/CI detection).
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            runtime_ctx: RuntimeContext::empty(),
            buffer_store: Arc::new(Mutex::new(OutputBufferStore::new())),
            shared_tools: None,
        }
    }

    /// Set the runtime context (project root, git, CI environment).
    pub fn with_runtime_context(mut self, ctx: RuntimeContext) -> Self {
        self.runtime_ctx = ctx;
        self
    }

    /// Inject a pre-created [`OutputBufferStore`] handle.
    ///
    /// Use this when the caller (e.g. the TUI) needs to hold a reference to
    /// the same store that the agent tools will write to, so that it can
    /// display live buffer status without going through the agent event channel.
    pub fn with_buffer_store(mut self, store: Arc<Mutex<OutputBufferStore>>) -> Self {
        self.buffer_store = store;
        self
    }

    /// Return a clone of the shared [`OutputBufferStore`] handle.
    ///
    /// Call this **before** or **after** `build()` to get a reference that can
    /// be polled by the TUI for live streaming display.
    pub fn buffer_store(&self) -> Arc<Mutex<OutputBufferStore>> {
        Arc::clone(&self.buffer_store)
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

    /// Build the [`Agent`] with the given mode, model, and tool-set profile.
    ///
    /// This method owns the creation of the shared mode lock and tool-event
    /// channel so that `SwitchModeTool` / `TodoWriteTool` and the agent loop
    /// operate on **the same** instances:
    ///
    /// 1. Creates `mode_lock` (same Arc for both the registry and the Agent).
    /// 2. Creates `(tool_event_tx, tool_event_rx)` (tx → tools, rx → Agent).
    /// 3. Converts [`RuntimeContext`] → [`AgentRuntimeContext`].
    /// 4. Builds a [`ToolRegistry`] via `build_tool_registry`.
    /// 5. Probes the provider for the actual context window (`GET /props`).
    /// 6. Constructs `Agent::new(...)`.
    pub async fn build(
        self,
        mode: AgentMode,
        model: Arc<dyn ModelProvider>,
        profile: ToolSetProfile,
    ) -> Agent {
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

        // Pass runtime.clone() as sub_agent_runtime so TaskTool sub-agents
        // inherit the parent's project root, AGENTS.md, CI/git context.
        let registry = build_tool_registry(
            &self.config,
            model.clone(),
            profile,
            mode_lock.clone(),
            tool_event_tx,
            runtime.clone(),
            Arc::clone(&self.buffer_store),
        );

        // Populate the shared tool snapshot so the TUI `/tools` inspector can
        // display all registered tools without accessing the registry directly.
        if let Some(ref st) = self.shared_tools {
            st.set(registry.schemas());
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

        Agent::new(
            model,
            Arc::new(registry),
            Arc::new(self.config.agent.clone()),
            runtime,
            mode_lock,
            tool_event_rx,
            context_window,
        )
    }
}
