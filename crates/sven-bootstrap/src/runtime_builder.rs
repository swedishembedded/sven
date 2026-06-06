// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`RuntimeBuilder`] — constructs a kernel-based runtime from config.
//!
//! This is the kernel-centric replacement for [`AgentBuilder`]: instead of
//! creating an `Agent` it produces an [`ErasedRuntime`] driven by a machine
//! fetched from [`sven_core::ModeRegistry`].
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

use sven_config::{Config, ModelConfig};
use sven_core::{Agent, ModeRegistry};
use sven_executors::{
    user::{ApprovalRequest, UserQuestion},
    CompositeExecutorBuilder,
};
use sven_hsm::{Context, ErasedRuntime, Event, EventSink, PermissionPolicy, RuntimeStatus};
use sven_llm::DefaultLlmAdapter;
use sven_mcp_client::{McpEvent, McpManager, McpTool};
use sven_model::Message;
use sven_tools::{PermissionRequester, QuestionRequest};
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{info, warn};

use crate::context::{RuntimeContext, ToolSetProfile};
use crate::registry::build_tool_registry;

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
}

// ── RuntimeBuilder ────────────────────────────────────────────────────────────

/// Constructs a kernel-based [`ErasedRuntime`] from a [`Config`].
///
/// Mirrors the API of [`AgentBuilder`] but produces the new HSM-kernel
/// runtime instead of the legacy `Agent`.
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
    /// Conversation history to seed into the converse agent before the
    /// first turn (used when resuming or switching sessions).
    initial_history: Vec<Message>,
    /// Optional permission requester for tool-call approval gating
    /// (e.g., ACP sends `session/request_permission` to the IDE).
    permission_requester: Option<Arc<dyn PermissionRequester>>,
    /// Shared abort slot wired into the `ConverseExecutor`. The TUI drops
    /// the sender (via `send_abort_signal`) to cancel an in-flight LLM turn.
    cancel_handle: Option<Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>>>,
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
        }
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

    /// Seed the converse agent with prior conversation history before the
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

    /// Provide the TUI's shared abort slot so the `ConverseExecutor` can
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

    /// Build the runtime. Returns the [`ErasedRuntime`], a cheap
    /// [`RuntimeHandle`] for posting events, the [`KernelChannels`] for
    /// the frontend, and the optional converse `Agent` (reactive mode only).
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The mode is not registered in [`ModeRegistry`].
    /// - The model provider cannot be initialised from config.
    pub async fn build(
        self,
    ) -> anyhow::Result<(
        ErasedRuntime,
        RuntimeHandle,
        KernelChannels,
        Option<Arc<Mutex<Agent>>>,
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
        let model_provider = sven_model::from_config(&model_cfg)?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model_provider);

        // ── MCP setup ────────────────────────────────────────────────────────
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
                let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
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

        // ── Build tool registry ───────────────────────────────────────────────
        // The reactive `agent`/`reactive` modes drive the full legacy agentic
        // loop through a `ConverseExecutor`; that requires building an `Agent`
        // (which owns the tool-event receiver and a shared mode lock).
        // "chat" uses the same converse streaming engine as "agent"/"reactive".
        let is_reactive = matches!(self.mode.as_str(), "agent" | "reactive" | "chat");

        let mode_lock = Arc::new(tokio::sync::Mutex::new(sven_config::AgentMode::Agent));
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

        // ── Optionally build the reactive Agent (converse engine) ─────────────
        let converse_agent: Option<Arc<Mutex<Agent>>> = if is_reactive {
            let context_window = match model.probe_context_window().await {
                Some(n) if n > 0 => n as usize,
                _ => model
                    .config_context_window()
                    .or_else(|| model.catalog_context_window())
                    .unwrap_or(128_000) as usize,
            };
            let resolver_config = Arc::clone(&self.config);
            let model_resolver: sven_core::ModelResolver = Arc::new(move |model_str: &str| {
                let model_cfg = sven_model::resolve_model_from_config(&resolver_config, model_str);
                let provider = sven_model::from_config(&model_cfg)?;
                Ok(Arc::from(provider) as Arc<dyn sven_model::ModelProvider>)
            });
            let mut agent = sven_core::Agent::new_with_params(sven_core::AgentNewParams {
                model: model.clone(),
                tools: tool_registry.clone(),
                config: Arc::new(self.config.agent.clone()),
                runtime,
                mode_lock: mode_lock.clone(),
                tool_event_rx,
                max_context_tokens: context_window,
                model_resolver: Some(model_resolver),
            });
            // Seed prior conversation history if provided (resume / session switch).
            if !self.initial_history.is_empty() {
                agent.seed_history(self.initial_history.clone()).await;
            }
            Some(Arc::new(Mutex::new(agent)))
        } else {
            drop(tool_event_rx);
            None
        };

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

        // ── Checkpoint dir ────────────────────────────────────────────────────
        let checkpoint_dir: PathBuf = self
            .runtime_ctx
            .project_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        // ── Build LLM adapter ─────────────────────────────────────────────────
        // Build a second model provider instance for the LlmAdapter (it takes
        // Box<dyn ModelProvider>, while the tool registry needs Arc).
        let llm_model_box =
            sven_model::from_config(&model_cfg).unwrap_or_else(|_| model_provider_for_llm(&model));
        let llm_adapter = Arc::new(DefaultLlmAdapter::new(llm_model_box));

        // ── Assemble executor ─────────────────────────────────────────────────
        let mut executor_builder = CompositeExecutorBuilder::default()
            .with_tools(tool_registry, Default::default())
            .with_user(question_tx, approval_tx)
            .with_timers(Arc::new(sven_hsm::SystemClock::new()))
            .with_checkpoints(checkpoint_dir)
            .with_audit(audit_log_path);
        // Keep a second Arc so the bundle can expose the agent to callers.
        let exposed_agent = converse_agent.clone();
        executor_builder = match converse_agent {
            // Reactive mode: drive the full agentic loop via the converse engine.
            Some(agent) => {
                let cancel_handle = self
                    .cancel_handle
                    .unwrap_or_else(|| Arc::new(tokio::sync::Mutex::new(None)));
                executor_builder.with_converse(agent, cancel_handle)
            }
            // Typed-JSON modes (chat/sdlc): use the structured LLM adapter.
            None => executor_builder.with_llm(llm_adapter),
        };
        let executor = executor_builder.build();

        // ── Permission policy (open: kernel gates per-state checks) ───────────
        let policy = PermissionPolicy::builder()
            .allow_globally([
                sven_hsm::ToolCapability::ReadFile,
                sven_hsm::ToolCapability::WriteFile,
                sven_hsm::ToolCapability::NetworkAccess,
                sven_hsm::ToolCapability::GitOperation,
            ])
            .build();

        // ── Spawn runtime ─────────────────────────────────────────────────────
        let erased_runtime = ErasedRuntime::spawn(machine, Context::new(), policy, executor, 64);

        let handle = RuntimeHandle {
            sink: erased_runtime.sink(),
            obs: erased_runtime.observations(),
            status_rx: erased_runtime.status_watch(),
        };

        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };

        Ok((erased_runtime, handle, channels, exposed_agent, mcp_manager, mcp_event_rx))
    }

    /// Build a fully-wired [`SessionBundle`] — the natural unit a
    /// [`SessionSupervisor`](crate::supervisor::SessionSupervisor) manages.
    ///
    /// This is a thin convenience wrapper over [`build`](Self::build) that
    /// packages the runtime, handle, channels, and optional converse agent
    /// into one owned value.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`build`](Self::build).
    pub async fn build_session(self) -> anyhow::Result<SessionBundle> {
        let (runtime, handle, channels, converse_agent, mcp_manager, mcp_event_rx) =
            self.build().await?;
        Ok(SessionBundle {
            runtime,
            handle,
            channels,
            converse_agent,
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
    /// The converse agent shared with the `ConverseExecutor` (reactive mode
    /// only). Expose so callers can seed history or swap the model without
    /// stopping the runtime.
    pub converse_agent: Option<Arc<Mutex<Agent>>>,
    /// MCP manager for the session. Frontends that display MCP slash-commands
    /// or toast notifications should call `McpManager::tools()` after startup
    /// and subscribe to `mcp_event_rx` for server events.
    pub mcp_manager: Arc<McpManager>,
    /// Receiver for MCP server events (tools changed, server health, etc.).
    /// Consume in the frontend or drop to silence.
    pub mcp_event_rx: mpsc::Receiver<McpEvent>,
}

/// Fallback: when `from_config` fails a second time, build a no-op provider
/// that always fails. This is only reached in misconfigured environments.
fn model_provider_for_llm(
    _model: &Arc<dyn sven_model::ModelProvider>,
) -> Box<dyn sven_model::ModelProvider> {
    // This path should never be reached in practice; `from_config` succeeds
    // twice with the same config unless the provider has ephemeral state.
    // Return a zero-dependency mock that always errors.
    struct NoOpProvider;
    #[async_trait::async_trait]
    impl sven_model::ModelProvider for NoOpProvider {
        fn name(&self) -> &str {
            "noop"
        }
        fn model_name(&self) -> &str {
            "noop"
        }
        async fn complete(
            &self,
            _req: sven_model::CompletionRequest,
        ) -> anyhow::Result<
            std::pin::Pin<
                Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
            >,
        > {
            anyhow::bail!("no model provider configured for LLM executor")
        }
    }
    Box::new(NoOpProvider)
}
