// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`RuntimeBuilder`] — constructs a kernel-based runtime from config.
//!
//! This is the single entry point for constructing a runtime: it produces an
//! [`ErasedRuntime`] driven by a machine fetched from
//! [`sven_machines::ModeRegistry`].
//!
//! # Usage
//!
//! ```rust,ignore
//! let (runtime, channels) = RuntimeBuilder::new(config)
//!     .with_mode("chat")
//!     .build()
//!     .await?;
//!
//! // Seed the machine with the first user message.
//! runtime.post(Event::UserMessage { text: "Hello".into() }).await;
//!
//! // Frontend holds the other end of the channels.
//! let question = channels.question_rx.recv().await;
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sven_config::{AgentMode, Config, ModelConfig};
use sven_executors::ThreadStore;
use sven_executors::{
    user::{ApprovalRequest, UserQuestion},
    CompositeExecutorBuilder, ToolExecutor, TurnExecutor,
};
use sven_hsm::{Context, ObservationSink, Principal, ToolCallId, UiEvent};
use sven_kernel::{EffectExecutor, ErasedRuntime};
use sven_machines::{ModeRegistry, SdlcMachine, UiTestMachine};
use sven_mcp_client::{McpEvent, McpManager, McpTool};
use sven_model::Message;
use sven_tool_api::events::ToolEvent;
use sven_tool_api::PermissionRequester;
use sven_tools_agent::QuestionRequest;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::context::{BuiltinTools, RuntimeContext, ToolSetProfile};
use crate::registry::{build_tool_registry_with_integrations, IntegrationProviders};
use crate::task_tool::ChildApprover;

/// Builds the executor installed in the composite's **tool slot** (`CallTool`),
/// given the `TurnExecutor`'s shared conversation store and `call_id → thread`
/// registry.
///
/// Installing a custom tool executor this way — instead of
/// [`RuntimeBuilder::with_effect_executor`] — keeps every other default
/// executor (turn/user/timer/audit) wired; only tool dispatch is
/// replaced. A deployment can pass a factory that shares the store with its
/// own executor so tool results append to the right thread. See
/// [`RuntimeBuilder::with_tool_executor_override`].
pub type ToolExecutorFactory = Box<
    dyn FnOnce(
            Arc<std::sync::Mutex<ThreadStore>>,
            Arc<std::sync::Mutex<std::collections::HashMap<ToolCallId, (String, String)>>>,
        ) -> Box<dyn EffectExecutor>
        + Send,
>;

pub use crate::session_handles::{KernelChannels, RuntimeHandle};

// ── RuntimeBuilder ────────────────────────────────────────────────────────────

/// Constructs a kernel-based [`ErasedRuntime`] from a [`Config`].
///
/// This is the sole builder for a wired runtime: it assembles the tool
/// registry, MCP manager, executors, and HSM machine into a running kernel.
pub struct RuntimeBuilder {
    config: Arc<Config>,
    mode: String,
    runtime_ctx: RuntimeContext,
    allow_interactive_oauth: bool,
    wait_for_mcp_tools_ms: Option<u64>,
    /// Per-session model config override (overrides `config.model`).
    model_cfg_override: Option<ModelConfig>,
    /// Tool-level question channel: forwarded into the tool registry so
    /// tools that call `ask_user` can route questions to the TUI modal.
    tool_question_tx: Option<mpsc::Sender<QuestionRequest>>,
    /// Conversation history to seed into the machine's thread before the
    /// first turn (used when resuming or switching sessions).
    initial_history: Vec<Message>,
    /// Optional permission requester for tool-call approval gating
    /// (e.g., ACP sends `session/request_permission` to the IDE).
    permission_requester: Option<Arc<dyn PermissionRequester>>,
    /// Shared abort slot wired into the `TurnExecutor`. The TUI drops
    /// the sender (via `send_abort_signal`) to cancel an in-flight LLM turn.
    cancel_handle: Option<Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>>,
    /// When set, this executor replaces the default [`CompositeExecutor`]
    /// entirely (see [`Self::with_effect_executor`]).
    effect_executor: Option<Box<dyn EffectExecutor>>,
    /// When set, this factory builds the executor for the composite's tool
    /// slot only (see [`Self::with_tool_executor_override`]); all other default
    /// executors stay wired.
    tool_executor_override: Option<ToolExecutorFactory>,
    /// Identity the session runs on behalf of (see [`Self::with_principal`]).
    /// `None` (the default) preserves the historical single-user behaviour.
    principal: Option<Principal>,
    /// When set, this provider is used instead of the one
    /// `sven_model_drivers::from_config` would construct (see
    /// [`Self::with_model_provider`]).
    model_provider_override: Option<Arc<dyn sven_model::ModelProvider>>,
    /// Kernel state to resume into instead of starting from the machine's
    /// initial state. See [`RuntimeBuilder::with_kernel_snapshot`].
    kernel_snapshot: Option<sven_hsm::Snapshot>,
    /// Domain facts seeded into the session's context before it starts.
    /// See [`RuntimeBuilder::with_context_facts`].
    context_facts: serde_json::Map<String, serde_json::Value>,
    /// See [`RuntimeBuilder::with_builtin_tools`].
    builtin_tools: BuiltinTools,
    /// Tools registered on top of the built-in set. See
    /// [`RuntimeBuilder::with_extra_tools`].
    extra_tools: Vec<Arc<dyn sven_tool_api::Tool>>,
    /// Mode registry to look the machine up in, when not the default one.
    /// See [`RuntimeBuilder::with_mode_registry`].
    mode_registry: Option<Arc<sven_machines::ModeRegistry>>,
    /// The interactive [`AgentMode`] this session runs as (see
    /// [`Self::with_agent_mode`]). Selects the permission policy for the
    /// reactive machine — `Plan`/`Research` get a read-only policy — and seeds
    /// the tool registry's mode lock. `None` preserves the historical
    /// full-agent behaviour.
    agent_mode: Option<AgentMode>,
    /// A pre-built, already-connected [`McpManager`] to reuse instead of
    /// creating a fresh one (see [`Self::with_mcp_manager`]). Interactive
    /// frontends share one manager across session rebuilds so the TUI's manager
    /// handle and MCP connections stay valid when mode/model changes.
    shared_mcp_manager: Option<Arc<McpManager>>,
}

impl RuntimeBuilder {
    /// Create a builder with the given configuration.
    ///
    /// `mode` selects the machine from [`ModeRegistry`] (e.g. `"chat"` or
    /// `"sdlc"`). Defaults to `"chat"` if empty.
    pub fn new(config: Arc<Config>, mode: impl Into<String>) -> Self {
        let mode = {
            let s = mode.into();
            if s.is_empty() {
                "chat".to_string()
            } else {
                s
            }
        };
        Self {
            config,
            mode,
            runtime_ctx: RuntimeContext::empty(),
            allow_interactive_oauth: true,
            wait_for_mcp_tools_ms: None,
            model_cfg_override: None,
            tool_question_tx: None,
            initial_history: Vec::new(),
            permission_requester: None,
            cancel_handle: None,
            effect_executor: None,
            tool_executor_override: None,
            principal: None,
            model_provider_override: None,
            kernel_snapshot: None,
            context_facts: serde_json::Map::new(),
            builtin_tools: BuiltinTools::default(),
            extra_tools: Vec::new(),
            mode_registry: None,
            agent_mode: None,
            shared_mcp_manager: None,
        }
    }

    /// Set the interactive [`AgentMode`] for this session.
    ///
    /// The mode selects the reactive machine's permission policy —
    /// `Plan`/`Research` build a read-only policy that forbids `WriteFile` at
    /// the kernel gate — and seeds the tool registry's mode lock. It does not
    /// change which kernel machine runs (that is the `mode` string passed to
    /// [`new`](Self::new)); `Plan` and `Agent` both drive the reactive machine.
    pub fn with_agent_mode(mut self, mode: AgentMode) -> Self {
        self.agent_mode = Some(mode);
        self
    }

    /// Reuse an existing, already-connected [`McpManager`] rather than
    /// constructing and connecting a fresh one.
    ///
    /// The manager's background tasks and server connections are assumed to be
    /// already running; `build` skips `connect_all`/`start_background_tasks` and
    /// the MCP-tool wait, and the returned MCP event receiver is a closed stub
    /// (the caller keeps consuming events from the original manager). Used by
    /// interactive frontends to keep one manager alive across session rebuilds.
    pub fn with_mcp_manager(mut self, manager: Arc<McpManager>) -> Self {
        self.shared_mcp_manager = Some(manager);
        self
    }

    /// Set the runtime context (project root, git, CI environment).
    pub fn with_runtime_context(mut self, ctx: RuntimeContext) -> Self {
        self.runtime_ctx = ctx;
        self
    }

    /// Disable interactive OAuth flows for headless/CI/batch runs.
    pub fn with_allow_interactive_oauth(mut self, allow: bool) -> Self {
        self.allow_interactive_oauth = allow;
        self
    }

    /// Wait up to `timeout_ms` ms for MCP tools before building.
    pub fn with_wait_for_mcp_tools(mut self, timeout_ms: u64) -> Self {
        self.wait_for_mcp_tools_ms = if timeout_ms > 0 {
            Some(timeout_ms)
        } else {
            None
        };
        self
    }

    /// Override the model config for this session (instead of `config.model`).
    pub fn with_model_config(mut self, cfg: ModelConfig) -> Self {
        self.model_cfg_override = Some(cfg);
        self
    }

    /// Provide a tool-level question sender so tools can route `ask_user`
    /// calls to the TUI question modal.
    pub fn with_tool_question_tx(mut self, tx: mpsc::Sender<QuestionRequest>) -> Self {
        self.tool_question_tx = Some(tx);
        self
    }

    /// Seed the machine's conversation thread with prior history before the
    /// first turn (used when resuming a saved session or switching between
    /// multi-session tabs).
    pub fn with_initial_history(mut self, messages: Vec<Message>) -> Self {
        self.initial_history = messages;
        self
    }

    /// Set a [`PermissionRequester`] that gates tool-call approval via an
    /// external channel (e.g. the ACP `session/request_permission` method).
    ///
    /// When set, the tool registry will call `requester.request_permission()`
    /// before executing any tool that has `ApprovalPolicy::Ask`.
    pub fn with_permission_requester(mut self, requester: Arc<dyn PermissionRequester>) -> Self {
        self.permission_requester = Some(requester);
        self
    }

    /// Provide the TUI's shared abort slot so the `TurnExecutor` can
    /// be cancelled via the existing `/abort` command.
    ///
    /// The slot is the same `Arc` held in `App::agent.cancel`. Before each
    /// LLM submission the executor stores its cancel sender there; the TUI's
    /// `send_abort_signal()` drops it to interrupt the in-flight call.
    pub fn with_cancel_handle(
        mut self,
        handle: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    ) -> Self {
        self.cancel_handle = Some(handle);
        self
    }

    /// Supply a custom [`EffectExecutor`] that replaces the default
    /// [`CompositeExecutor`](sven_executors::CompositeExecutor) entirely.
    ///
    /// The kernel hands **every** validated effect to `exec`; none of the
    /// default sub-executors (turn/tool/user/timer/audit) are
    /// wired. In particular the [`KernelChannels`] question/approval
    /// receivers will observe closed channels, since the default
    /// `UserExecutor` that feeds them is not installed.
    ///
    /// Intended for embedding sven's kernel with bespoke I/O (e.g. managed
    /// cloud agents) and for tests that assert on emitted effects.
    pub fn with_effect_executor(mut self, exec: Box<dyn EffectExecutor>) -> Self {
        self.effect_executor = Some(exec);
        self
    }

    /// Override only the composite's **tool slot** (`CallTool`), leaving every
    /// other default executor (turn/user/timer/audit) wired.
    ///
    /// Unlike [`Self::with_effect_executor`] — which drops the entire default
    /// composite — this substitutes just the tool executor. The `factory` is
    /// handed the `TurnExecutor`'s shared [`ThreadStore`] and
    /// `call_id → thread` registry so a custom executor can append tool
    /// results to the exact thread the turn engine reads on its continuation
    /// call.
    ///
    /// Ignored when [`Self::with_effect_executor`] is also set (the wholesale
    /// override wins).
    pub fn with_tool_executor_override(mut self, factory: ToolExecutorFactory) -> Self {
        self.tool_executor_override = Some(factory);
        self
    }

    /// Set the [`Principal`] (tenant + actor + roles) the session runs on
    /// behalf of. It is seeded into the kernel [`Context`] so every dispatch
    /// audit record is stamped with the tenant and actor ids.
    pub fn with_principal(mut self, principal: Principal) -> Self {
        self.principal = Some(principal);
        self
    }

    /// Supply the [`sven_model::ModelProvider`] the kernel talks to instead
    /// of letting the builder construct one from config.
    ///
    /// This is the structural seam gateway wrappers hang off: a deployment
    /// builds the config provider itself (`sven_model_drivers::from_config`),
    /// wraps it, and injects the wrapped provider here — no kernel path can
    /// then reach the model unwrapped. Unlike [`Self::with_effect_executor`], all default
    /// executors (turn/tool/user/timer/audit) stay wired.
    pub fn with_model_provider(mut self, provider: Box<dyn sven_model::ModelProvider>) -> Self {
        self.model_provider_override = Some(Arc::from(provider));
        self
    }

    /// Supply a provider that is *shared* with other sessions.
    ///
    /// [`Self::with_model_provider`] hands the builder sole ownership, which
    /// means one provider - and one HTTP client, connection pool and set of
    /// credentials - per session. An engine serving many concurrent agents
    /// wants the opposite: build the provider once and lend it to every
    /// session it constructs.
    pub fn with_shared_model_provider(
        mut self,
        provider: Arc<dyn sven_model::ModelProvider>,
    ) -> Self {
        self.model_provider_override = Some(provider);
        self
    }

    /// Register tools on top of the built-in set.
    ///
    /// This is how an application gives its agents capabilities the kernel has
    /// never heard of. They are registered after the built-ins and after MCP
    /// tools, so a name collision resolves in the caller's favour.
    pub fn with_extra_tools(mut self, tools: Vec<Arc<dyn sven_tool_api::Tool>>) -> Self {
        self.extra_tools = tools;
        self
    }

    /// Look the mode's machine up in `registry` instead of the default one.
    ///
    /// The extension point for a machine that does not live in this workspace:
    /// a caller builds a registry from
    /// [`ModeRegistry::default_registry`](sven_machines::ModeRegistry::default_registry),
    /// registers its own factory, and passes it here.
    pub fn with_mode_registry(mut self, registry: Arc<sven_machines::ModeRegistry>) -> Self {
        self.mode_registry = Some(registry);
        self
    }

    /// Seed domain facts into the session's context before it starts.
    ///
    /// The kernel keeps a machine's knowledge in `Context::facts`, which is how
    /// a machine is parameterised without teaching the kernel anything about
    /// the domain. A caller that needs a machine to start out knowing something
    /// - the JSON schema a `predict` turn must conform to, say - puts it here.
    ///
    /// Facts seeded this way are applied on top of a resumed snapshot's own
    /// facts, so a caller can re-seed per step without losing what the machine
    /// accumulated.
    pub fn with_context_facts(mut self, facts: serde_json::Map<String, serde_json::Value>) -> Self {
        self.context_facts = facts;
        self
    }

    /// Selects the built-in tools this session registers. Defaults to
    /// [`BuiltinTools::Detect`]; [`BuiltinTools::None`] also connects no MCP
    /// servers, so the session has exactly the tools passed to
    /// [`Self::with_extra_tools`].
    #[must_use]
    pub fn with_builtin_tools(mut self, selection: BuiltinTools) -> Self {
        self.builtin_tools = selection;
        self
    }

    /// Resume the kernel into a previously captured state instead of starting
    /// the machine from scratch.
    ///
    /// This is what makes a session survive being put down: a service can
    /// suspend an agent between requests and rebuild it here without replaying
    /// its event log. The snapshot's context - facts, granted capabilities,
    /// audit trail - becomes the session's initial context.
    ///
    /// Building fails if the mode's machine does not enumerate the snapshot's
    /// state (see `Machine::all_states`).
    pub fn with_kernel_snapshot(mut self, snapshot: sven_hsm::Snapshot) -> Self {
        self.kernel_snapshot = Some(snapshot);
        self
    }

    /// Build the runtime. Returns the [`ErasedRuntime`], a cheap
    /// [`RuntimeHandle`] for posting events, the [`KernelChannels`] for
    /// the frontend, the [`McpManager`], and the MCP event receiver.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The mode is not registered in [`ModeRegistry`].
    /// - The model provider cannot be initialised from config.
    pub async fn build(
        mut self,
    ) -> anyhow::Result<(
        ErasedRuntime,
        RuntimeHandle,
        KernelChannels,
        Arc<McpManager>,
        mpsc::Receiver<McpEvent>,
    )> {
        // ── Look up machine ───────────────────────────────────────────────────
        let registry = match self.mode_registry.take() {
            Some(supplied) => supplied,
            None => Arc::new(ModeRegistry::default_registry()),
        };
        let factory = registry.get(&self.mode).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown mode {:?}; available: {:?}",
                self.mode,
                registry.modes()
            )
        })?;
        let machine = factory();

        // ── Initialise model provider ─────────────────────────────────────────
        let model_cfg = self
            .model_cfg_override
            .clone()
            .unwrap_or(self.config.model.clone());
        // An injected provider (see `with_model_provider`) wins over the
        // config-constructed one — that is how metering/gateway wrappers get
        // between the kernel and the model.
        let model: Arc<dyn sven_model::ModelProvider> = match self.model_provider_override.take() {
            Some(provider) => provider,
            // `_probed` additionally asks the live server for its actual
            // context window (a short-timeout, best-effort HTTP call) and
            // clamps a hand-written `max_tokens` down to what the server can
            // really serve — this is what stops sven from building a prompt
            // the server will reject after already admitting the request.
            // `build()` already does I/O two lines below (MCP connect), so
            // this adds no new purity concern; it is a builder, not a
            // transition.
            None => Arc::from(sven_model_drivers::from_config_probed(&model_cfg).await?),
        };

        // ── MCP setup ────────────────────────────────────────────────────────
        // Reuse a caller-supplied manager (session rebuild) instead of building
        // and connecting a fresh one, so the frontend's manager handle and MCP
        // connections survive a mid-session mode/model change. A reused manager
        // yields a closed event-receiver stub — the caller keeps consuming from
        // the original manager's stream.
        let (mcp_manager, mcp_event_rx) = match self.shared_mcp_manager.take() {
            Some(existing) => {
                let (_tx, rx) = tokio::sync::mpsc::channel(1);
                (existing, rx)
            }
            // No built-in tools means no configured MCP servers either: the
            // caller asked for exactly the tools it registers.
            None if self.builtin_tools == BuiltinTools::None => {
                let (mcp_event_tx, mcp_event_rx) = tokio::sync::mpsc::channel(1);
                let manager = McpManager::new(Default::default(), mcp_event_tx, false);
                (manager, mcp_event_rx)
            }
            None => {
                let (mcp_event_tx, mcp_event_rx) = tokio::sync::mpsc::channel(64);
                let mcp_manager = McpManager::new(
                    self.config.mcp_servers.clone(),
                    mcp_event_tx,
                    self.allow_interactive_oauth,
                );
                mcp_manager.connect_all().await;
                mcp_manager.start_background_tasks();

                let has_enabled_servers = self.config.mcp_servers.values().any(|c| c.enabled);
                if let Some(timeout_ms) = self.wait_for_mcp_tools_ms {
                    if has_enabled_servers {
                        let deadline =
                            tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
                        let poll_interval = Duration::from_millis(200);
                        loop {
                            let tools = mcp_manager.tools().await;
                            if !tools.is_empty() {
                                info!(
                                    count = tools.len(),
                                    "MCP tools available for kernel runtime"
                                );
                                break;
                            }
                            if tokio::time::Instant::now() >= deadline {
                                warn!(timeout_ms, "MCP tools not available within timeout");
                                break;
                            }
                            tokio::time::sleep(poll_interval).await;
                        }
                    }
                }
                (mcp_manager, mcp_event_rx)
            }
        };

        // ── Build tool registry ───────────────────────────────────────────────
        // Seed the mode lock from the interactive mode so in-session `/mode`
        // reads and mode-scoped tool views reflect the active mode.
        let mode_lock = Arc::new(tokio::sync::Mutex::new(
            self.agent_mode.unwrap_or(sven_config::AgentMode::Agent),
        ));
        let (tool_event_tx, tool_event_rx) =
            tokio::sync::mpsc::channel::<sven_tool_api::events::ToolEvent>(64);
        let mut runtime = self.runtime_ctx.to_agent_runtime();
        runtime.append_system_prompt = self.runtime_ctx.append_system_prompt;
        runtime.system_prompt_override = self.runtime_ctx.system_prompt_override;
        runtime.no_system = self.runtime_ctx.no_system;
        runtime.no_tools = self.runtime_ctx.no_tools;

        let todos = Arc::new(tokio::sync::Mutex::new(Vec::<
            sven_tool_api::events::TodoItem,
        >::new()));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(
            sven_tools_fs::OutputBufferStore::new(),
        ));

        // ── User/approval channels ────────────────────────────────────────────
        // Created before the tools, so a `task` sub-agent's permission
        // requests reach this session's own approval gate (or the host's
        // requester, when it brought one).
        let (question_tx, question_rx) = mpsc::channel::<UserQuestion>(16);
        let (approval_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(16);
        let approver = match self.permission_requester.clone() {
            Some(host) => ChildApprover::Host(host),
            None => ChildApprover::Gate(Arc::new(crate::session_handles::GateApprover::new(
                approval_tx.clone(),
            ))),
        };
        // Semantic memory (SQLite + FTS5 `semantic_memory` tool) is
        // constructed here, the one real assembly point every surface
        // (headless CI, interactive TUI, ACP, MCP) goes through, so it is on
        // by default without any surface having to opt in. `open` fails only
        // on a broken `$HOME`/on-disk state (permissions, corruption); that
        // is not fatal to the session, so the tool is simply left
        // unregistered and the reason logged, exactly like a missing MCP
        // tool.
        #[allow(unused_mut)]
        let mut integration_providers = IntegrationProviders {
            approver: Some(approver),
            ..IntegrationProviders::default()
        };
        #[cfg(feature = "memory")]
        {
            integration_providers.memory_store = match sven_memory::SqliteMemoryStore::open(None)
                .await
            {
                Ok(store) => Some(Arc::new(store) as Arc<dyn sven_memory::VectorStore>),
                Err(err) => {
                    warn!(
                        error = %err,
                        "failed to open semantic memory store; semantic_memory tool will not be registered"
                    );
                    None
                }
            };
        }

        let mode = self.agent_mode.unwrap_or(sven_config::AgentMode::Agent);
        let root = self.runtime_ctx.project_root.as_deref();
        let q = self.tool_question_tx.clone();
        let tool_profile =
            ToolSetProfile::for_selection(self.builtin_tools, mode, root, q, todos, buffer_store);
        // The registry describes the model this session actually runs on, which
        // an override may have replaced (sub-agents are told to use it).
        let registry_config = Config {
            model: model_cfg.clone(),
            ..(*self.config).clone()
        };
        let mut tool_registry = match tool_profile {
            Some(profile) => build_tool_registry_with_integrations(
                &registry_config,
                model.clone(),
                profile,
                mode_lock.clone(),
                tool_event_tx,
                runtime.clone(),
                integration_providers,
            ),
            None => sven_tool_registry::ToolRegistry::new(),
        };

        let mcp_tools: Vec<McpTool> = mcp_manager.tools().await;
        for tool in mcp_tools {
            tool_registry.register(tool);
        }

        // Caller-supplied tools go last so a name collision resolves in their
        // favour: an application that deliberately shadows a built-in has said
        // something, and silently ignoring it would be the surprising choice.
        for tool in std::mem::take(&mut self.extra_tools) {
            tool_registry.register_arc(tool);
        }

        // Last, so a disabled name is gone whoever registered it.
        for name in &self.config.tools.disabled {
            tool_registry.remove(name);
        }

        if let Some(requester) = self.permission_requester {
            tool_registry.set_permission_requester(requester);
        }

        let ask_question_available = tool_registry.get("ask_question").is_some();
        let tool_registry = Arc::new(tool_registry);
        // Clone for the RuntimeHandle before the registry is moved into the
        // ToolExecutor below (frontends reach it for MCP tool hot-swap).
        let tool_registry_for_handle = Arc::clone(&tool_registry);

        // `tool_event_rx` is forwarded onto the kernel's `ObservationSink` once
        // one exists — see the `spawn_tool_event_forwarder` call below, after
        // `erased_runtime` is spawned. It is *not* dropped here; TurnExecutor
        // does not read `ToolEvent`s itself (only the kernel's own `UiEvent`
        // plane), but several tools (`task`, `todo`, `system`) still report
        // through this side channel — see that function's doc comment for the
        // full history.

        // ── Shared TurnExecutor resources ─────────────────────────────────────
        // Conversation store and call-id registry are shared between TurnExecutor
        // and ToolExecutor so appended tool results can be retrieved per-thread.
        let conv_store = Arc::new(std::sync::Mutex::new(ThreadStore::new()));
        // Clone for the RuntimeHandle before the store is moved into the
        // ToolExecutor below (frontends reach it for history seeding / resume).
        let conv_store_for_handle = Arc::clone(&conv_store);
        // Seed the system message, then prior conversation history, into the
        // thread this mode's machine actually reads, so a fresh, resumed, or
        // piped session sees the full context on its very first turn. Seeding
        // the wrong thread is silent - the machine just starts with nothing -
        // which is why the mapping lives in `sven_machines::mode` next to the
        // registry rather than being assumed here. The SDLC machine uses
        // per-phase threads and simply ignores this seed. Seeding happens before any dispatch, so
        // the append-only cache-safety invariant of the thread is preserved
        // (system → history → new user turn). `build_system_message` returns
        // `None` when `--no-system` was given with no override/append text,
        // in which case no system message is appended at all - zero tokens
        // spent before the first message. `initial_history` never itself
        // contains a system-role message by convention (parsers that seed it
        // strip the system message into `system_prompt_override` instead), so
        // there is no risk of a duplicate.
        let seed_thread = sven_machines::mode::primary_thread(&self.mode);
        if let Ok(mut store) = conv_store.lock() {
            let mode = self.agent_mode.unwrap_or(sven_config::AgentMode::Agent);
            if let Some(system_msg) = runtime.build_system_message(mode) {
                store.append(seed_thread, system_msg);
            }
            for msg in &self.initial_history {
                store.append(seed_thread, msg.clone());
            }
        }
        let call_id_to_thread = Arc::new(std::sync::Mutex::new(std::collections::HashMap::<
            ToolCallId,
            (String, String),
        >::new()));

        // A parked question's durable record is the same regardless of which
        // surface is driving this session (TUI, headless, ACP), unlike
        // question_tx/approval_tx above (which need a UI to actually collect
        // a reply) - so the drain lives here, once, rather than being pushed
        // out to every surface to wire up itself.
        #[cfg(feature = "memory")]
        let (parked_tx, mut parked_rx) = sven_executors::UserExecutor::parked_channel(16);
        #[cfg(feature = "memory")]
        tokio::spawn(async move {
            let ledger = sven_memory::QuestionLedger::at_default_path();
            while let Some(q) = parked_rx.recv().await {
                let asked_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if let Err(err) = ledger.record_asked(&sven_memory::QuestionAskedRecord {
                    question_id: q.question_id,
                    call_id: q.call_id,
                    prompt: q.prompt,
                    options: q.options,
                    asked_at,
                }) {
                    tracing::warn!(
                        error = %err,
                        "failed to record a parked question; it will not appear in `sven questions list`"
                    );
                }
            }
        });

        // ── Audit log path ────────────────────────────────────────────────────
        let audit_log_path: PathBuf = self
            .runtime_ctx
            .project_root
            .as_ref()
            .map(|r| r.join(".sven").join("audit.jsonl"))
            .unwrap_or_else(|| PathBuf::from(".sven/audit.jsonl"));

        // Shared mirror of the kernel's audit trail: the runtime syncs it
        // around every dispatch and then executes `PersistAudit` itself, so
        // the AuditExecutor flushes the full hash-chained records after each
        // dispatch (machines never emit `PersistAudit`).
        //
        // Concurrent sessions on the same project root share this log file;
        // the AuditExecutor serializes appends via an advisory lock on a
        // `<log>.lock` sidecar and re-reads the chain tip under it, so
        // interleaved sessions extend one linear, verifiable chain. Records
        // of different tenants are intermingled in the shared file but every
        // dispatch and tool record carries tenant/actor attribution, so
        // per-tenant views can be filtered out of the log.
        let audit_trail = sven_hsm::AuditTrailHandle::new();

        // ── Verifier root ─────────────────────────────────────────────────────
        let verify_root: PathBuf = self
            .runtime_ctx
            .project_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        // ── Assemble executor ─────────────────────────────────────────────────
        let cancel_handle = self
            .cancel_handle
            .clone()
            .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));

        // Parallel-execution fan-out: in SDLC mode install a child spawner so
        // the kernel can service `Effect::InstantiateSubmachine` by running each
        // decomposed task as an isolated concurrent child kernel.
        let is_sdlc = self.mode.as_str() == "sdlc";
        let child_spawner: Option<Arc<dyn sven_kernel::ChildSpawner>> = if is_sdlc {
            Some(Arc::new(
                crate::child_spawner::SdlcChildSpawner::new(
                    model.clone(),
                    Arc::clone(&self.config),
                    Arc::clone(&tool_registry),
                )
                .with_gates(question_tx.clone(), approval_tx.clone()),
            ))
        } else {
            None
        };

        // ── TurnExecutor (kernel-native single-turn LLM+tools loop) ──────────
        // All modes now use TurnExecutor for kind="turn" effects.
        // Clone the Arcs so ToolExecutor can share the same backing store and
        // call-id registry — tool results must be appended under the exact thread
        // and with the original LLM-assigned call id before the continuation call.
        let turn_executor = TurnExecutor::new(
            model.clone(),
            None,
            Arc::clone(&tool_registry),
            Arc::clone(&conv_store),
            Arc::clone(&call_id_to_thread),
            cancel_handle.clone(),
        )
        .with_no_tools(runtime.no_tools)
        .with_compaction_config(sven_executors::CompactionConfig::from_agent_config(
            &self.config.agent,
        ))
        .with_turn_limits(sven_turn::TurnLimits::from_agent_config(&self.config.agent));

        // A caller-supplied executor (see `with_effect_executor`) replaces the
        // default composite wholesale; otherwise wire the default composite,
        // substituting only the tool slot when a tool-executor override is set
        // (see `with_tool_executor_override`).
        let executor: Box<dyn EffectExecutor> = match self.effect_executor {
            Some(custom) => custom,
            None => {
                let user_executor = sven_executors::UserExecutor::new(question_tx, approval_tx);
                #[cfg(feature = "memory")]
                let user_executor = user_executor.with_parked_questions(parked_tx);
                let base = CompositeExecutorBuilder::default()
                    .with_user_slot(Box::new(user_executor))
                    .with_timers(Arc::new(sven_kernel::SystemClock::new()))
                    .with_audit_trail(audit_log_path, audit_trail.clone())
                    .with_turn(turn_executor)
                    // Same root every other effect that touches the
                    // filesystem uses; only the verified-task machine's
                    // `Verifying` state ever emits `Effect::Verify` (see its
                    // `permission_policy`), so wiring it here unconditionally
                    // is harmless for every other mode.
                    .with_verify(verify_root);
                let composed = match self.tool_executor_override.take() {
                    Some(factory) => base.with_tool_slot(factory(
                        Arc::clone(&conv_store),
                        Arc::clone(&call_id_to_thread),
                    )),
                    None => base.with_tool_executor(
                        ToolExecutor::with_shared_store(
                            tool_registry,
                            Default::default(),
                            Arc::clone(&call_id_to_thread),
                            Arc::clone(&conv_store),
                        )
                        .with_no_tools(runtime.no_tools)
                        .with_tool_result_token_cap(self.config.agent.tool_result_token_cap),
                    ),
                };
                Box::new(composed.build())
            }
        };

        // ── Permission policy — per-machine real policy ───────────────────────
        // Use the machine's declared policy so the kernel enforces capability
        // restrictions per state (e.g. SDLC disallows writes outside Execution).
        let policy = match self.mode.as_str() {
            "sdlc" => SdlcMachine::permission_policy(),
            "verified-task" => sven_machines::VerifiedTaskMachine::permission_policy(),
            "ui-test" => UiTestMachine::permission_policy(),
            _ => crate::mode_policy::reactive_policy(self.agent_mode.unwrap_or(AgentMode::Agent)),
        };

        // ── Spawn runtime ─────────────────────────────────────────────────────
        // Seed the parallel-execution flag so the SDLC machine only fans out
        // when a child spawner is actually installed (otherwise it stays
        // single-track and never emits orphaned InstantiateSubmachine effects).
        // A resumed session starts from the snapshotted state and context; a
        // fresh one starts from the machine's initial state and an empty
        // context. Restoring before spawning is what keeps the runtime from
        // re-entering the initial state and re-running completed work.
        let mut machine = machine;
        let mut init_ctx = match self.kernel_snapshot.take() {
            Some(snapshot) => {
                machine.restore_state(&snapshot.state)?;
                snapshot.context
            }
            None => Context::new(),
        };
        // The configured round budget, unless the caller set its own.
        init_ctx.set_fact(
            sven_machines::MAX_TOOL_ROUNDS_FACT,
            serde_json::json!(self.config.agent.max_tool_rounds),
        );
        for (key, value) in std::mem::take(&mut self.context_facts) {
            init_ctx.facts.insert(key, value);
        }
        // What the registry holds is a fact about this session, not a caller
        // preference, so it is set after the caller's facts.
        init_ctx.set_fact(
            sven_machines::ASK_QUESTION_AVAILABLE_FACT,
            serde_json::json!(ask_question_available),
        );
        init_ctx.principal = self.principal.clone();
        if child_spawner.is_some() {
            init_ctx.set_fact("parallel_execution", serde_json::json!(true));
        }
        let erased_runtime = ErasedRuntime::spawn_with_audit_trail(
            machine,
            init_ctx,
            policy,
            executor,
            64,
            child_spawner,
            audit_trail,
        );

        // Forward the legacy `ToolEvent` side channel onto the kernel's own
        // `ObservationSink` now that one exists (`erased_runtime.observations()`
        // is only available after the runtime is spawned, which is why this
        // isn't done up where `tool_event_rx` was created). See
        // `spawn_tool_event_forwarder`'s doc comment.
        spawn_tool_event_forwarder(tool_event_rx, erased_runtime.observations());

        let handle = RuntimeHandle {
            sink: erased_runtime.sink(),
            obs: erased_runtime.observations(),
            status_rx: erased_runtime.status_watch(),
            conv_store: conv_store_for_handle,
            tool_registry: tool_registry_for_handle,
        };

        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };

        Ok((erased_runtime, handle, channels, mcp_manager, mcp_event_rx))
    }

    /// Build a fully-wired [`SessionBundle`] — the natural unit a
    /// [`SessionSupervisor`](crate::supervisor::SessionSupervisor) manages.
    ///
    /// This is a thin convenience wrapper over [`build`](Self::build) that
    /// packages the runtime, handle, channels, and MCP manager into one owned
    /// value.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`build`](Self::build).
    pub async fn build_session(self) -> anyhow::Result<SessionBundle> {
        let (runtime, handle, channels, mcp_manager, mcp_event_rx) = self.build().await?;
        Ok(SessionBundle {
            runtime,
            handle,
            channels,
            mcp_manager,
            mcp_event_rx,
        })
    }
}

// ── SessionBundle ─────────────────────────────────────────────────────────────

/// All the moving parts of one live kernel session.
///
/// Owns the [`ErasedRuntime`] (keeping the consumer task alive), the cheap
/// [`RuntimeHandle`] for posting events and subscribing to observations, and
/// the [`KernelChannels`] carrying user-question / approval requests.
pub struct SessionBundle {
    /// The running kernel. Dropping it shuts the session down.
    pub runtime: ErasedRuntime,
    /// Cheap, cloneable handle for posting events / subscribing to UiEvents.
    pub handle: RuntimeHandle,
    /// Question / approval request receivers for the frontend.
    pub channels: KernelChannels,
    /// MCP manager for the session. Frontends that display MCP slash-commands
    /// or toast notifications should call `McpManager::tools()` after startup
    /// and subscribe to `mcp_event_rx` for server events.
    pub mcp_manager: Arc<McpManager>,
    /// Receiver for MCP server events (tools changed, server health, etc.).
    /// Consume in the frontend or drop to silence.
    pub mcp_event_rx: mpsc::Receiver<McpEvent>,
}

// ── Legacy ToolEvent → UiEvent forwarding ───────────────────────────────────

/// Forward [`ToolEvent`]s onto the kernel's [`ObservationSink`] as the
/// matching [`UiEvent`], for as long as `rx`'s sender half is alive.
///
/// # Why this exists
///
/// Tools report side-band state changes through
/// `sven_tool_api::events::ToolEvent` — `TodoTool` (todo list updates),
/// `TaskTool` (`SubagentStarted`/`SubagentEvent`), `SystemTool`
/// (`ModeChanged`). `TurnExecutor` only reads the kernel's own `UiEvent`
/// observation plane, so without this forwarder those events would reach no
/// surface (TUI, CI, ACP, SDK) — `RuntimeBuilder` is the one assembly point
/// every one of them goes through.
///
/// This function re-threads the side channel onto the observation plane,
/// mapping each `ToolEvent` onto the `UiEvent` variant that reports the same
/// fact (`AgentEvent`/`UiEvent` are the same `sven_vocab::SessionEvent`
/// type, so a `ToolEvent` and the equivalent `AgentEvent` produce the same
/// value).
///
/// `ToolEvent::McpServerAdded`/`McpServerRemoved` are deliberately **not**
/// forwarded — they are registry mutations (add/remove a tool from the live
/// `ToolRegistry`), not renderable observations, and have no `UiEvent`
/// counterpart.
fn spawn_tool_event_forwarder(mut rx: mpsc::Receiver<ToolEvent>, obs: ObservationSink) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let ui_event = match event {
                ToolEvent::TodoUpdate(items) => UiEvent::TodoUpdate(items),
                ToolEvent::ModeChanged(mode) => UiEvent::ModeChanged(mode),
                ToolEvent::ModelChanged(m) => UiEvent::ModelChanged(m),
                ToolEvent::Progress { call_id, message } => {
                    UiEvent::ToolProgress { call_id, message }
                }
                ToolEvent::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                } => UiEvent::DelegateSummary {
                    to_name,
                    task_title,
                    duration_ms,
                    status,
                    result_preview,
                },
                ToolEvent::SubagentStarted {
                    call_id,
                    handle_id,
                    description,
                    prompt,
                } => UiEvent::SubagentStarted {
                    call_id,
                    handle_id,
                    description,
                    prompt,
                },
                ToolEvent::SubagentEvent {
                    call_id,
                    handle_id,
                    update,
                } => UiEvent::SubagentEvent {
                    call_id,
                    handle_id,
                    update,
                },
                // No `UiEvent` counterpart — see the doc comment above.
                ToolEvent::McpServerAdded { .. } | ToolEvent::McpServerRemoved(_) => continue,
            };
            obs.emit(ui_event);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sven_hsm::{Effect, EffectKind, Event, ObservationSink};
    use sven_kernel::EventSink;

    use super::*;

    /// Forwards every effect it receives to a channel so the test can assert
    /// the kernel routed effects to the injected executor.
    struct RecordingExecutor {
        tx: mpsc::UnboundedSender<Effect>,
    }

    #[async_trait::async_trait]
    impl EffectExecutor for RecordingExecutor {
        async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
            let _ = self.tx.send(effect);
        }
    }

    #[tokio::test]
    async fn custom_effect_executor_is_injected_and_receives_effects() {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let (tx, mut rx) = mpsc::unbounded_channel::<Effect>();

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .with_effect_executor(Box::new(RecordingExecutor { tx }))
                .build()
                .await
                .expect("runtime should build with a custom executor");

        handle.send_user_message("hello".into()).await;

        // The chat machine reacts to UserMessage with a CallLlm(kind=turn)
        // effect; it must reach the injected executor (skip any earlier
        // init/audit effects).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut saw_call_llm = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(effect)) => {
                    if effect.kind() == EffectKind::CallLlm {
                        saw_call_llm = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        runtime.abort();
        assert!(
            saw_call_llm,
            "injected custom executor never received the CallLlm effect"
        );
    }

    fn mock_config() -> Config {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        config
    }

    #[tokio::test]
    async fn a_disabled_tool_is_never_registered() {
        let mut config = mock_config();
        config.tools.disabled = vec!["shell".into()];
        let (runtime, handle, ..) = RuntimeBuilder::new(Arc::new(config), "agent")
            .with_builtin_tools(BuiltinTools::Coding)
            .build()
            .await
            .expect("runtime builds");
        let names = handle.tool_registry().names();
        runtime.abort();
        assert!(names.iter().any(|n| n == "read_file"), "{names:?}");
        assert!(!names.iter().any(|n| n == "shell"), "{names:?}");
    }

    #[tokio::test]
    async fn the_configured_round_budget_reaches_the_machine() {
        let mut config = mock_config();
        config.agent.max_tool_rounds = 7;
        let (runtime, ..) = RuntimeBuilder::new(Arc::new(config), "agent")
            .build()
            .await
            .expect("runtime builds");
        let snapshot = runtime.capture().await.expect("runtime is live");
        runtime.abort();
        assert_eq!(
            snapshot.context.fact(sven_machines::MAX_TOOL_ROUNDS_FACT),
            Some(&serde_json::json!(7))
        );
    }

    /// A minimal tool-slot executor: records every effect it receives (there
    /// should only ever be `CallTool`, since the composite routes everything
    /// else elsewhere) and answers with a synthetic `ToolSucceeded` so the
    /// turn loop can continue to its second round.
    struct MockToolExecutor {
        tx: mpsc::UnboundedSender<Effect>,
    }

    #[async_trait::async_trait]
    impl EffectExecutor for MockToolExecutor {
        async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
            let Effect::CallTool { call_id, .. } = &effect else {
                let _ = self.tx.send(effect);
                return;
            };
            let call_id = *call_id;
            let _ = self.tx.send(effect);
            let _ = sink
                .emit(Event::ToolSucceeded {
                    call_id,
                    observation: serde_json::Value::String("mock tool ran".into()),
                })
                .await;
        }
    }

    /// `with_tool_executor_override` installs a custom executor as just the
    /// composite's **tool slot** — `CallTool` reaches it, but the rest of the
    /// default composite (turn/user/timer/audit) stays wired, so a
    /// scripted tool-then-text turn still completes its second round.
    #[tokio::test]
    async fn tool_executor_override_owns_call_tool_rest_stays_on_default_composite() {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let (tx, mut rx) = mpsc::unbounded_channel::<Effect>();
        let provider = sven_model_mock::ScriptedMockProvider::tool_then_text(
            "call-1",
            "write_file",
            "{\"path\":\"mock_probe.txt\",\"text\":\"x\",\"append\":false}",
            "done",
        );
        let last_request = provider.last_request.clone();

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .with_model_provider(Box::new(provider))
                .with_tool_executor_override(Box::new(|_store, _call_id_to_thread| {
                    Box::new(MockToolExecutor { tx })
                }))
                .build()
                .await
                .expect("runtime should build with a tool-executor override");

        handle.send_user_message("hello".into()).await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let effect = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .ok()
            .flatten();
        assert!(
            matches!(effect, Some(Effect::CallTool { .. })),
            "tool-slot override must receive CallTool, got {effect:?}"
        );

        // The default TurnExecutor is still wired: after the tool result, it
        // sends the continuation call that produces round 2 ("done").
        while tokio::time::Instant::now() < deadline && last_request.lock().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runtime.abort();
        assert!(
            last_request.lock().unwrap().is_some(),
            "default turn executor never sent the continuation call after the tool result"
        );
    }

    #[tokio::test]
    async fn injected_model_provider_replaces_config_construction() {
        // A config whose provider `from_config` cannot build: if the build
        // succeeds, the injected provider — not the config — was used. This
        // is the seam gateway wrappers hang off, so it must structurally
        // bypass config construction.
        let mut config = Config::default();
        config.model.provider = "no-such-provider".into();
        config.model.name = "ghost".into();
        let config = Arc::new(config);

        assert!(
            RuntimeBuilder::new(Arc::clone(&config), "chat")
                .build()
                .await
                .is_err(),
            "sanity: the config alone must fail provider construction"
        );

        let (runtime, _handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(config, "chat")
                .with_model_provider(Box::new(sven_model_mock::MockProvider))
                .build()
                .await
                .expect("an injected provider must bypass from_config");
        runtime.abort();
    }

    #[tokio::test]
    async fn principal_is_stamped_into_dispatch_audit_records() {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        // A recording executor keeps the kernel free of real I/O; the audit
        // trail is written by the pure dispatch engine regardless.
        let (tx, mut rx) = mpsc::unbounded_channel::<Effect>();

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .with_principal(sven_hsm::Principal::new("acme", "alice"))
                .with_effect_executor(Box::new(RecordingExecutor { tx }))
                .build()
                .await
                .expect("runtime should build");

        handle.send_user_message("hello".into()).await;

        // Once an effect reaches the executor, the dispatch that produced it
        // has already appended its audit record.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let _ = tokio::time::timeout_at(deadline, rx.recv()).await;

        let audit = runtime.audit_snapshot();
        runtime.abort();
        assert!(!audit.is_empty(), "dispatch must append audit records");
        for record in &audit {
            assert_eq!(record.tenant_id.as_deref(), Some("acme"));
            assert_eq!(record.actor_id.as_deref(), Some("alice"));
        }
    }

    /// `semantic_memory` must be registered by default (no opt-in wiring
    /// required) once the `memory` feature is compiled in — today it is
    /// dead code: `IntegrationProviders::default()` never populates
    /// `memory_store`, so no caller of `RuntimeBuilder::build` ever sees it.
    #[tokio::test]
    async fn semantic_memory_is_present_in_the_default_registry() {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .build()
                .await
                .expect("runtime should build with the default registry");

        let present = handle.tool_registry().get("semantic_memory").is_some();
        runtime.abort();
        assert!(
            present,
            "semantic_memory must be registered in the default tool registry"
        );
    }

    /// Drive one turn through the default composite executor with a scripted
    /// provider and return the exact request it received, so `--no-system`
    /// wiring can be asserted end-to-end (not just at the `PromptContext`
    /// composition level covered by `sven_machines::runtime_context` unit tests).
    async fn first_request_with(runtime_ctx: RuntimeContext) -> sven_model::CompletionRequest {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let provider = sven_model_mock::ScriptedMockProvider::always_text("hi");
        let last_request = provider.last_request.clone();

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .with_runtime_context(runtime_ctx)
                .with_model_provider(Box::new(provider))
                .build()
                .await
                .expect("runtime should build");

        handle.send_user_message("hello".into()).await;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if last_request.lock().unwrap().is_some() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        runtime.abort();

        let req = last_request
            .lock()
            .unwrap()
            .clone()
            .expect("the model must have received a request");
        req
    }

    /// A project with no embedded-debugging configuration must not pay for
    /// the GDB and large-content tools on every single request.
    ///
    /// `ToolSetProfile::detect` has always decided this -- it checks for
    /// `.gdbinit`/`openocd.cfg`/`debugging/` and picks `Coding` when none is
    /// present -- and was called by nothing but its own unit tests, because
    /// `build()` hard-coded `Full`. Every session on every surface (TUI,
    /// headless, ACP, and every sub-agent) therefore carried `gdb` and
    /// `context`: 4,203 characters of schema, roughly 1,100 tokens, in a
    /// repository that has never run a debugger.
    #[tokio::test]
    async fn a_project_without_gdb_config_does_not_load_the_gdb_tools() {
        let req = first_request_with(RuntimeContext::empty()).await;
        let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();

        assert!(
            !names.contains(&"gdb"),
            "no .gdbinit here, so no gdb tool: {names:?}"
        );
        assert!(
            !names.contains(&"context"),
            "large-content tools are opt-in too: {names:?}"
        );
        // The point is to drop what is unused, not to break the session.
        for expected in ["read_file", "edit_file", "grep", "shell", "task"] {
            assert!(
                names.contains(&expected),
                "{expected} must survive: {names:?}"
            );
        }
    }

    #[tokio::test]
    async fn default_session_seeds_a_system_message() {
        let req = first_request_with(RuntimeContext::empty()).await;
        assert_eq!(
            req.messages.first().map(|m| m.role.clone()),
            Some(sven_model::Role::System)
        );
    }

    #[tokio::test]
    async fn no_system_alone_sends_zero_system_messages() {
        let mut ctx = RuntimeContext::empty();
        ctx.no_system = true;
        let req = first_request_with(ctx).await;
        assert!(
            req.messages
                .iter()
                .all(|m| m.role != sven_model::Role::System),
            "--no-system with no override/append must add zero system tokens"
        );
    }

    #[tokio::test]
    async fn no_system_with_override_sends_exactly_the_override() {
        let mut ctx = RuntimeContext::empty();
        ctx.no_system = true;
        ctx.system_prompt_override = Some("Exact prompt.".to_string());
        let req = first_request_with(ctx).await;
        let system_msgs: Vec<_> = req
            .messages
            .iter()
            .filter(|m| m.role == sven_model::Role::System)
            .collect();
        assert_eq!(system_msgs.len(), 1);
        assert_eq!(system_msgs[0].as_text(), Some("Exact prompt."));
    }

    #[tokio::test]
    async fn default_session_sends_nonempty_tool_schemas() {
        let req = first_request_with(RuntimeContext::empty()).await;
        assert!(
            !req.tools.is_empty(),
            "a default session must advertise its tool set"
        );
    }

    #[tokio::test]
    async fn no_tools_sends_zero_tool_schemas() {
        let mut ctx = RuntimeContext::empty();
        ctx.no_tools = true;
        let req = first_request_with(ctx).await;
        assert!(
            req.tools.is_empty(),
            "--no-tools must send zero tool schemas regardless of mode"
        );
    }

    // The defence-in-depth guarantee - a tool call is refused even if a model
    // somehow attempts one despite seeing no schemas - is covered at the
    // focused unit level in `sven_executors::tool::tests`
    // (`no_tools_refuses_a_call_without_running_the_tool`), which can observe
    // the ToolExecutor's `Event::ToolFailed` output directly instead of
    // racing the kernel's observation bus.
}
