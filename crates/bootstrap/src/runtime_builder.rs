// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`RuntimeBuilder`] — constructs a kernel-based runtime from config.
//!
//! This is the single entry point for constructing a runtime: it produces an
//! [`ErasedRuntime`] driven by a machine fetched from
//! [`sven_core::ModeRegistry`].
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
use sven_core::{ModeRegistry, ReactiveAgentMachine, SdlcMachine};
use sven_executors::{
    user::{ApprovalRequest, UserQuestion},
    CompositeExecutorBuilder, ToolExecutor, TurnExecutor,
};
use sven_hsm::{
    Context, EffectExecutor, ErasedRuntime, Event, EventSink, ObservationSink, Principal,
    RuntimeStatus, ToolCallId, UiEvent,
};
use sven_llm::ConversationStore;
use sven_mcp_client::{McpEvent, McpManager, McpTool};
use sven_model::Message;
use sven_tools::events::ToolEvent;
use sven_tools::{PermissionRequester, QuestionRequest, ToolRegistry};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use crate::context::{RuntimeContext, ToolSetProfile};
use crate::registry::build_tool_registry;

/// Builds the executor installed in the composite's **tool slot** (`CallTool`),
/// given the `TurnExecutor`'s shared conversation store and `call_id → thread`
/// registry.
///
/// Installing a custom tool executor this way — instead of
/// [`RuntimeBuilder::with_effect_executor`] — keeps every other default
/// executor (turn/user/timer/checkpoint/audit) wired; only tool dispatch is
/// replaced. Managed-cloud deployments pass a factory that shares the store
/// with a `sven_executors::RemoteToolExecutor` so remote tool results append to
/// the right thread. See [`RuntimeBuilder::with_tool_executor_override`].
pub type ToolExecutorFactory = Box<
    dyn FnOnce(
            Arc<std::sync::Mutex<ConversationStore>>,
            Arc<std::sync::Mutex<std::collections::HashMap<ToolCallId, (String, String)>>>,
        ) -> Box<dyn EffectExecutor>
        + Send,
>;

// ── KernelChannels ────────────────────────────────────────────────────────────

/// Channel endpoints returned to the caller (TUI / node / CI) so they can
/// exchange user input and approval decisions with the running kernel.
pub struct KernelChannels {
    /// Receives questions the kernel's `UserExecutor` forwards from
    /// `Effect::AskUser`. The holder must display the prompt and send the
    /// answer through [`UserQuestion::reply_tx`].
    pub question_rx: mpsc::Receiver<UserQuestion>,
    /// Receives approval requests forwarded from
    /// `Effect::RequestHumanApproval`. The holder must approve or deny via
    /// [`ApprovalRequest::reply_tx`].
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
}

// ── RuntimeHandle ─────────────────────────────────────────────────────────────

/// A cheap-to-clone handle to a spawned [`ErasedRuntime`].
///
/// Provides the event sink and status watch; the caller typically also holds
/// the [`KernelChannels`] returned alongside this handle.
#[derive(Clone)]
pub struct RuntimeHandle {
    sink: EventSink,
    obs: sven_hsm::ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    /// The kernel's shared conversation store (thread → turns). Exposed so
    /// interactive frontends can seed / replace history mid-session for the
    /// edit-resubmit and resume flows.
    conv_store: Arc<std::sync::Mutex<ConversationStore>>,
    /// The live tool registry. Exposed so frontends can hot-swap MCP tools via
    /// [`ToolRegistry::replace_mcp_tools`] without rebuilding the session.
    tool_registry: Arc<ToolRegistry>,
}

impl RuntimeHandle {
    /// Returns a cloneable sink for posting events into the kernel.
    #[must_use]
    pub fn sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// Returns a clone of the outward observation sink for this session.
    #[must_use]
    pub fn observations(&self) -> sven_hsm::ObservationSink {
        self.obs.clone()
    }

    /// Subscribes a fresh receiver to the outward observation plane
    /// (`UiEvent` stream: streamed text, tool progress, usage, transitions).
    #[must_use]
    pub fn subscribe_observations(&self) -> tokio::sync::broadcast::Receiver<sven_hsm::UiEvent> {
        self.obs.subscribe()
    }

    /// Posts `Event::UserMessage { text }` into the kernel queue.
    pub async fn send_user_message(&self, text: String) -> bool {
        self.sink.emit(Event::UserMessage { text }).await
    }

    /// Posts `Event::UserCancelled` into the kernel queue.
    pub async fn cancel(&self) -> bool {
        self.sink.emit(Event::UserCancelled).await
    }

    /// The latest published status snapshot.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.status_rx.borrow().clone()
    }

    /// A fresh receiver for status updates (watch channel).
    #[must_use]
    pub fn status_watch(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_rx.clone()
    }

    /// The kernel's shared conversation store (for history seeding / resume).
    #[must_use]
    pub fn conversation_store(&self) -> Arc<std::sync::Mutex<ConversationStore>> {
        Arc::clone(&self.conv_store)
    }

    /// A snapshot of the reactive-agent conversation thread.
    ///
    /// Used to carry accumulated context forward when a session is rebuilt
    /// (e.g. on a mid-session model switch) so the replacement kernel can be
    /// seeded with the same history. Empty if the store mutex is poisoned.
    #[must_use]
    pub fn history_snapshot(&self) -> Vec<Message> {
        self.conv_store
            .lock()
            .ok()
            .map(|store| store.snapshot(sven_core::machines::reactive_agent::CHAT_THREAD))
            .unwrap_or_default()
    }

    /// Replace the reactive-agent conversation thread with `messages`.
    ///
    /// The history-seeding hook a rebuilt kernel uses so the next turn streams
    /// against exactly those turns. A no-op if the store mutex is poisoned.
    pub fn seed_history(&self, messages: Vec<Message>) {
        if let Ok(mut store) = self.conv_store.lock() {
            store.replace_thread(sven_core::machines::reactive_agent::CHAT_THREAD, messages);
        }
    }

    /// The live tool registry (for MCP tool hot-swap).
    #[must_use]
    pub fn tool_registry(&self) -> Arc<ToolRegistry> {
        Arc::clone(&self.tool_registry)
    }
}

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
    /// `sven_model::from_config` would construct (see
    /// [`Self::with_model_provider`]).
    model_provider_override: Option<Box<dyn sven_model::ModelProvider>>,
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
    /// calls to the TUI/GUI question modal.
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
    /// default sub-executors (turn/tool/user/timer/checkpoint/audit) are
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
    /// other default executor (turn/user/timer/checkpoint/audit) wired.
    ///
    /// Unlike [`Self::with_effect_executor`] — which drops the entire default
    /// composite — this substitutes just the tool executor. The `factory` is
    /// handed the `TurnExecutor`'s shared [`ConversationStore`] and
    /// `call_id → thread` registry so a custom executor (e.g.
    /// `sven_executors::RemoteToolExecutor::with_shared_store`) can append tool
    /// results to the exact thread the turn engine reads on its continuation
    /// call. This is how managed-cloud sessions route `CallTool` to a customer's
    /// companion while the LLM turn loop keeps running in the cloud.
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
    /// This is the structural seam gateway wrappers hang off: managed-cloud
    /// deployments build the config provider themselves
    /// (`sven_model::from_config`), wrap it (e.g. in `sven-cloud`'s
    /// `MeteredProvider` so every turn is priced and debited live), and
    /// inject the wrapped provider here — no kernel path can then reach the
    /// model unmetered. Unlike [`Self::with_effect_executor`], all default
    /// executors (turn/tool/user/timer/checkpoint/audit) stay wired.
    pub fn with_model_provider(mut self, provider: Box<dyn sven_model::ModelProvider>) -> Self {
        self.model_provider_override = Some(provider);
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
        let registry = ModeRegistry::default_registry();
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
        let model_provider = match self.model_provider_override.take() {
            Some(provider) => provider,
            // `_probed` additionally asks the live server for its actual
            // context window (a short-timeout, best-effort HTTP call) and
            // clamps a hand-written `max_tokens` down to what the server can
            // really serve — this is what stops sven from building a prompt
            // the server will reject after already admitting the request.
            // `build()` already does I/O two lines below (MCP connect), so
            // this adds no new purity concern; it is a builder, not a
            // transition.
            None => sven_model::from_config_probed(&model_cfg).await?,
        };
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model_provider);

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
                                info!(count = tools.len(), "MCP tools available for kernel runtime");
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
            tokio::sync::mpsc::channel::<sven_tools::events::ToolEvent>(64);
        let mut runtime = self.runtime_ctx.to_agent_runtime();
        runtime.append_system_prompt = self.runtime_ctx.append_system_prompt;
        runtime.system_prompt_override = self.runtime_ctx.system_prompt_override;

        let todos = Arc::new(tokio::sync::Mutex::new(
            Vec::<sven_tools::events::TodoItem>::new(),
        ));
        let buffer_store = Arc::new(tokio::sync::Mutex::new(sven_tools::OutputBufferStore::new()));

        let mut tool_registry = build_tool_registry(
            &self.config,
            model.clone(),
            ToolSetProfile::Full {
                question_tx: self.tool_question_tx.clone(),
                todos,
                buffer_store,
            },
            mode_lock.clone(),
            tool_event_tx,
            runtime.clone(),
        );

        let mcp_tools: Vec<McpTool> = mcp_manager.tools().await;
        for tool in mcp_tools {
            tool_registry.register(tool);
        }

        if let Some(requester) = self.permission_requester {
            tool_registry.set_permission_requester(requester);
        }

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
        let conv_store = Arc::new(std::sync::Mutex::new(ConversationStore::new()));
        // Clone for the RuntimeHandle before the store is moved into the
        // ToolExecutor below (frontends reach it for history seeding / resume).
        let conv_store_for_handle = Arc::clone(&conv_store);
        // Seed prior conversation history into the reactive-agent thread so a
        // resumed or piped session sees the full context on its very first turn.
        // Only the reactive `agent`/`chat` machines read `CHAT_THREAD`; the SDLC
        // machine uses per-phase threads and simply ignores this seed. Seeding
        // happens before any dispatch, so the append-only cache-safety invariant
        // of the thread is preserved (empty → history → new user turn).
        if !self.initial_history.is_empty() {
            if let Ok(mut store) = conv_store.lock() {
                for msg in &self.initial_history {
                    store.append(
                        sven_core::machines::reactive_agent::CHAT_THREAD,
                        msg.clone(),
                    );
                }
            }
        }
        let call_id_to_thread = Arc::new(std::sync::Mutex::new(
            std::collections::HashMap::<ToolCallId, (String, String)>::new(),
        ));

        // ── User/approval channels ────────────────────────────────────────────
        let (question_tx, question_rx) = mpsc::channel::<UserQuestion>(16);
        let (approval_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(16);

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

        // ── Checkpoint dir ────────────────────────────────────────────────────
        let checkpoint_dir: PathBuf = self
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
        let child_spawner: Option<Arc<dyn sven_hsm::ChildSpawner>> = if is_sdlc {
            Some(Arc::new(crate::child_spawner::SdlcChildSpawner::new(
                model.clone(),
                Arc::clone(&self.config),
                Arc::clone(&tool_registry),
            )))
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
        );

        // A caller-supplied executor (see `with_effect_executor`) replaces the
        // default composite wholesale; otherwise wire the default composite,
        // substituting only the tool slot when a tool-executor override is set
        // (see `with_tool_executor_override`).
        let executor: Box<dyn EffectExecutor> = match self.effect_executor {
            Some(custom) => custom,
            None => {
                let base = CompositeExecutorBuilder::default()
                    .with_user(question_tx, approval_tx)
                    .with_timers(Arc::new(sven_hsm::SystemClock::new()))
                    .with_checkpoints(checkpoint_dir)
                    .with_audit_trail(audit_log_path, audit_trail.clone())
                    .with_turn(turn_executor);
                let composed = match self.tool_executor_override.take() {
                    Some(factory) => base.with_tool_slot(factory(
                        Arc::clone(&conv_store),
                        Arc::clone(&call_id_to_thread),
                    )),
                    None => base.with_tool_executor(ToolExecutor::with_shared_store(
                        tool_registry,
                        Default::default(),
                        Arc::clone(&call_id_to_thread),
                        Arc::clone(&conv_store),
                    )),
                };
                Box::new(composed.build())
            }
        };

        // ── Permission policy — per-machine real policy ───────────────────────
        // Use the machine's declared policy so the kernel enforces capability
        // restrictions per state (e.g. SDLC disallows writes outside Execution).
        let policy = match self.mode.as_str() {
            "sdlc" => SdlcMachine::permission_policy(),
            // Read-only planning modes get a policy that withholds `WriteFile`
            // so the kernel forbids file mutations even if the model proposes
            // one; all other modes keep the full reactive-agent policy.
            _ => match self.agent_mode {
                Some(AgentMode::Plan | AgentMode::Research) => {
                    ReactiveAgentMachine::plan_permission_policy()
                }
                _ => ReactiveAgentMachine::permission_policy(),
            },
        };

        // ── Spawn runtime ─────────────────────────────────────────────────────
        // Seed the parallel-execution flag so the SDLC machine only fans out
        // when a child spawner is actually installed (otherwise it stays
        // single-track and never emits orphaned InstantiateSubmachine effects).
        let mut init_ctx = Context::new();
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
/// `sven_tools::events::ToolEvent` predates the HSM kernel: it was how tools
/// reported side-band state changes (todo list updates, mode/model switches,
/// subagent lifecycle, delegate summaries) back to the old `sven_core::Agent`
/// loop. Several tools still send through it — `TodoTool`, `TaskTool`
/// (`SubagentStarted`/`SubagentEvent`), `SystemTool` (`ModeChanged`) — but
/// [`build`](RuntimeBuilder::build) used to just `drop` the receiver ("all
/// modes now use `TurnExecutor`"), on the assumption that `TurnExecutor` had
/// a replacement path for all of it. It doesn't: `TurnExecutor` only reads
/// the kernel's own `UiEvent` observation plane, which nothing was ever
/// posting these to. The result was silent, total data loss — not just the
/// `task`-tool `AgentEvent::SubagentStarted`/`SubagentEvent` this module was
/// changed to fix (see `crates/ci/src/runner/event.rs`), but todo-list
/// updates and mode changes reported by tools too, on every surface (TUI,
/// GUI, CI, node, ACP), since `RuntimeBuilder` is the one assembly point
/// every one of them goes through.
///
/// This function is the fix: it re-threads the side channel onto the
/// observation plane using the same field-for-field mapping
/// `sven_executors::turn::agent_event_to_ui` already uses for the equivalent
/// `AgentEvent` variants, so a `ToolEvent` and an `AgentEvent` reporting the
/// same fact produce the identical `UiEvent`.
///
/// `ToolEvent::McpServerAdded`/`McpServerRemoved` are deliberately **not**
/// forwarded — they are registry mutations (add/remove a tool from the live
/// `ToolRegistry`), not renderable observations, and have no `UiEvent`
/// counterpart; wiring those up is a separate change (hot MCP registry
/// reload), out of scope here.
fn spawn_tool_event_forwarder(mut rx: mpsc::Receiver<ToolEvent>, obs: ObservationSink) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let ui_event = match event {
                ToolEvent::TodoUpdate(items) => UiEvent::TodoUpdate(
                    serde_json::to_value(&items).unwrap_or(serde_json::Value::Null),
                ),
                ToolEvent::ModeChanged(mode) => UiEvent::ModeChanged(format!("{mode:?}")),
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
                    update: serde_json::to_value(&update).unwrap_or(serde_json::Value::Null),
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

    use sven_hsm::{Effect, EffectKind, ObservationSink};

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

    #[tokio::test]
    async fn injected_model_provider_replaces_config_construction() {
        // A config whose provider `from_config` cannot build: if the build
        // succeeds, the injected provider — not the config — was used. This
        // is the seam metering gateways (sven-cloud's MeteredProvider) hang
        // off, so it must structurally bypass config construction.
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
                .with_model_provider(Box::new(sven_model::MockProvider))
                .build()
                .await
                .expect("an injected provider must bypass from_config");
        runtime.abort();
    }

    #[tokio::test]
    async fn remote_tool_executor_installs_via_effect_executor_hook() {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        // RemoteToolExecutor owns CallTool; everything else must flow to its
        // delegate. A recording delegate stands in for the default composite.
        let (tx, mut rx) = mpsc::unbounded_channel::<Effect>();
        let (to_companion_tx, _to_companion_rx) = mpsc::channel(8);
        let exec = sven_executors::RemoteToolExecutor::new(to_companion_tx)
            .with_delegate(Box::new(RecordingExecutor { tx }));

        let (runtime, handle, _channels, _mcp_manager, _mcp_event_rx) =
            RuntimeBuilder::new(Arc::new(config), "chat")
                .with_effect_executor(Box::new(exec))
                .build()
                .await
                .expect("runtime should build with a RemoteToolExecutor");

        handle.send_user_message("hello".into()).await;

        // The chat machine reacts with a CallLlm effect — not owned by the
        // remote tool executor, so it must reach the delegate.
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
            "RemoteToolExecutor must delegate non-CallTool effects to the composite"
        );
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
}

