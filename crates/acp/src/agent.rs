// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`SvenAcpAgent`] - implements the ACP `Agent` trait for sven.
//!
//! Each `new_session` call builds a fresh kernel session via
//! [`sven_bootstrap::RuntimeBuilder`] and wraps it in a
//! [`sven_bootstrap::KernelAgentSession`] — the shared HSM-kernel adapter that
//! maps the kernel's outward observation plane onto the same
//! [`AgentEvent`](sven_machines::AgentEvent) stream every other surface consumes.
//! The session is stored in a [`SessionEntry`] keyed by ACP [`SessionId`];
//! `set_session_mode` rebuilds it in the new mode from the session's
//! settings, so the mode's policy and tools - not only its label - apply; it
//! is refused while a prompt turn runs.
//! `prompt` posts the user message through the session, drains the mapped
//! `AgentEvent` stream, and bridges each event to an ACP `session/update`
//! notification, returning when the turn completes or is cancelled.
//!
//! The struct is intentionally `!Send` (it uses `RefCell` for interior
//! mutability) and lives inside a `tokio::task::LocalSet` spawned by
//! [`crate::serve_stdio`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    AgentCapabilities, AuthenticateRequest, AuthenticateResponse, CancelNotification, Error,
    ErrorCode, InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PermissionOption, PermissionOptionKind, PromptCapabilities, PromptRequest, PromptResponse,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    Result as AcpResult, SelectedPermissionOutcome, SessionMode, SessionModeId, SessionModeState,
    SessionNotification, SetSessionModeRequest, SetSessionModeResponse, StopReason, ToolCallUpdate,
    ToolCallUpdateFields,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

/// How long `send_notification` waits for the I/O background task to flush a
/// `session/update` notification before giving up.
///
/// The background task calls `conn.session_notification(...).await`, which
/// writes to stdout.  If the IDE stops reading stdout the write stalls and
/// the ack never arrives.  Rather than blocking the entire `prompt()` future
/// (and therefore the whole LocalSet) indefinitely, we time-out and continue
/// streaming - the IDE will have to cope with the dropped notification.
const NOTIFY_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait, by default, for the IDE to respond to a
/// `session/request_permission` request before defaulting to denial.
pub const DEFAULT_PERMISSION_TIMEOUT: Duration = Duration::from_secs(60);

use sven_bootstrap::session_handles::HumanGate;
use sven_bootstrap::{KernelAgentSession, RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, ApprovalMode, Config};
use sven_hsm::ToolCapability;
use sven_machines::AgentEvent;

/// `_meta` key under which a permission request names the capability the call
/// exercises, as the agent's kernel classed it (`"WriteFile"`, ...). A `task`
/// parent answering its sub-agent reads it; a client may ignore it.
pub use sven_bootstrap::task_tool::CAPABILITY_META_KEY;

use crate::bridge::{
    acp_mode_id_to_sven_mode, agent_event_to_session_update, sven_mode_to_acp_mode_id,
};

// ─── Version string ───────────────────────────────────────────────────────────

const SVEN_VERSION: &str = env!("CARGO_PKG_VERSION");

// ─── Inter-task messaging ─────────────────────────────────────────────────────

/// Messages sent from the `Agent` trait methods to the background task that
/// owns the [`AgentSideConnection`] so it can call `conn.session_notification`
/// or `conn.request_permission`.
pub enum ConnMessage {
    SessionUpdate(SessionNotification, oneshot::Sender<()>),
    /// Request IDE permission for a tool call.  The background task calls
    /// `conn.request_permission(request)` and sends the result back.
    RequestPermission {
        request: RequestPermissionRequest,
        response_tx: oneshot::Sender<RequestPermissionResponse>,
    },
}

// ─── AcpPermissionRequester ───────────────────────────────────────────────────

/// Implements [`sven_tool_api::PermissionRequester`] by forwarding permission
/// requests to the IDE over ACP via the `session/request_permission` method.
///
/// Created per session in [`SvenAcpAgent::new_session`] and passed to
/// [`RuntimeBuilder::with_permission_requester`] so that tools with
/// `ApprovalPolicy::Ask` gate their execution on an explicit IDE approval.
struct AcpPermissionRequester {
    session_id: String,
    conn_tx: mpsc::UnboundedSender<ConnMessage>,
    /// How long to wait for the answer before denying the call. Always
    /// bounded: a client that never answers must not stall the session.
    timeout: Duration,
}

#[async_trait::async_trait]
impl sven_tool_api::PermissionRequester for AcpPermissionRequester {
    async fn request_permission(
        &self,
        call: &sven_tool_api::ToolCall,
        capability: ToolCapability,
    ) -> bool {
        self.ask(call, capability).await
    }
}

impl AcpPermissionRequester {
    /// Asks the client about `call`, naming `capability` in the request's
    /// `_meta`; `false` unless the client allows it within the timeout.
    async fn ask(&self, call: &sven_tool_api::ToolCall, capability: ToolCapability) -> bool {
        // Clone all borrowed data up-front so the future is 'static and Send.
        let call_id = call.id.clone();
        let call_name = call.name.clone();
        let call_args = call.args.clone();
        let session_id = self.session_id.clone();
        let conn_tx = self.conn_tx.clone();

        // A unit variant always serializes, to its name.
        let meta = serde_json::to_value(capability).ok().map(|cap| {
            let mut meta = serde_json::Map::new();
            meta.insert(CAPABILITY_META_KEY.to_string(), cap);
            meta
        });
        let tool_call_update = ToolCallUpdate::new(
            call_id,
            ToolCallUpdateFields::new()
                .title(call_name.clone())
                .raw_input(call_args),
        )
        .meta(meta);

        let allow_once_id = "allow_once";
        let reject_once_id = "reject_once";

        let options = vec![
            PermissionOption::new(allow_once_id, "Allow once", PermissionOptionKind::AllowOnce),
            PermissionOption::new(reject_once_id, "Reject", PermissionOptionKind::RejectOnce),
        ];

        let allow_ids: HashSet<String> = options
            .iter()
            .filter(|o| {
                matches!(
                    o.kind,
                    PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways
                )
            })
            .map(|o| o.option_id.0.to_string())
            .collect();

        let request = RequestPermissionRequest::new(session_id, tool_call_update, options);

        let (response_tx, response_rx) = oneshot::channel();
        if conn_tx
            .send(ConnMessage::RequestPermission {
                request,
                response_tx,
            })
            .is_err()
        {
            warn!(
                tool = %call_name,
                "ACP permission request failed: conn_tx closed - denying"
            );
            return false;
        }

        let answered = tokio::time::timeout(self.timeout, response_rx).await;
        match answered {
            Ok(Ok(response)) => match &response.outcome {
                RequestPermissionOutcome::Selected(SelectedPermissionOutcome {
                    option_id, ..
                }) => allow_ids.contains(&option_id.0.to_string()),
                RequestPermissionOutcome::Cancelled | _ => false,
            },
            Ok(Err(_)) => {
                warn!(tool = %call_name, "ACP permission response channel dropped - denying");
                false
            }
            Err(_) => {
                warn!(
                    tool = %call_name,
                    "ACP permission request timed out after {}s - denying",
                    self.timeout.as_secs()
                );
                false
            }
        }
    }
}

// ─── Turn usage ───────────────────────────────────────────────────────────────

/// The tokens a prompt turn used, summed from its `TokenUsage` events.
#[derive(Default, Debug, PartialEq, Eq)]
struct TurnUsage {
    input: u64,
    output: u64,
}

impl TurnUsage {
    fn add(&mut self, event: &AgentEvent) {
        if let AgentEvent::TokenUsage { input, output, .. } = event {
            self.input += u64::from(*input);
            self.output += u64::from(*output);
        }
    }

    fn into_acp(self) -> agent_client_protocol::Usage {
        agent_client_protocol::Usage::new(self.input + self.output, self.input, self.output)
    }
}

// ─── Session entry ────────────────────────────────────────────────────────────

/// What a session is set up with. Every build of the session - the first,
/// and each rebuild for a mode or model change - is made from exactly this.
#[derive(Clone, Debug)]
struct SessionSettings {
    /// The working directory the session was opened in.
    cwd: std::path::PathBuf,
    mode: AgentMode,
    /// The model the session runs on: the configured one until the agent
    /// switches (`system` switch_model).
    model: sven_config::ModelConfig,
}

/// Per-session state stored inside [`SvenAcpAgent`].
struct SessionEntry {
    /// Kernel session bridged onto the shared [`AgentEvent`] stream; owns the
    /// runtime and keeps the kernel alive for the session's lifetime.
    /// Replaced when the mode or model changes, never during a turn.
    session: tokio::sync::Mutex<KernelAgentSession>,
    settings: std::sync::Mutex<SessionSettings>,
    /// The agent switched models during a turn: the session is rebuilt on
    /// the new one before the next turn starts.
    model_changed: AtomicBool,
    /// The session's MCP servers, those the agent added included, kept
    /// across rebuilds.
    mcp_manager: Arc<sven_bootstrap::McpManager>,
    /// How many `prompt` calls are running or waiting on this session. A
    /// rebuild happens only while it is zero.
    turns_in_flight: AtomicUsize,
    /// Where every build of the session sends its events.
    event_tx: mpsc::Sender<AgentEvent>,
    /// Receiver for this session's mapped [`AgentEvent`] stream, drained one
    /// turn at a time by `prompt`.
    event_rx: tokio::sync::Mutex<mpsc::Receiver<AgentEvent>>,
    /// Cancellation sender; replaced on each new prompt turn.
    cancel_tx: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl SessionEntry {
    fn settings(&self) -> SessionSettings {
        self.settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Counts a `prompt` as in flight until the returned guard drops.
    fn begin_turn(&self) -> TurnInFlight<'_> {
        self.turns_in_flight.fetch_add(1, Ordering::SeqCst);
        TurnInFlight(&self.turns_in_flight)
    }
}

/// A `prompt` running on a session; see [`SessionEntry::begin_turn`].
struct TurnInFlight<'a>(&'a AtomicUsize);

impl Drop for TurnInFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

// ─── SvenAcpAgent ─────────────────────────────────────────────────────────────

/// ACP agent implementation backed by the HSM kernel.
///
/// `!Send` due to `RefCell`; must run inside a [`tokio::task::LocalSet`].
pub struct SvenAcpAgent {
    config: Arc<Config>,
    sessions: RefCell<HashMap<String, Arc<SessionEntry>>>,
    conn_tx: mpsc::UnboundedSender<ConnMessage>,
    permission_timeout: Duration,
    approval: ApprovalMode,
}

impl SvenAcpAgent {
    pub fn new(config: Arc<Config>, conn_tx: mpsc::UnboundedSender<ConnMessage>) -> Self {
        Self {
            config,
            sessions: RefCell::new(HashMap::new()),
            conn_tx,
            permission_timeout: DEFAULT_PERMISSION_TIMEOUT,
            approval: ApprovalMode::Auto,
        }
    }

    /// Runs every session under `approval`. Under [`ApprovalMode::Manual`]
    /// each call that is not read-only is put to the client
    /// (`session/request_permission`) - how a `task` parent under manual
    /// approval sees its sub-agent's calls. Under auto only a tool whose
    /// policy is `Ask` is put to the client: the host's own policy. Every
    /// request carries the call's capability under [`CAPABILITY_META_KEY`].
    #[must_use]
    pub fn with_approval_mode(mut self, approval: ApprovalMode) -> Self {
        self.approval = approval;
        self
    }

    /// Builds the kernel session for `session_id` from `settings`, seeded
    /// with `history`, bridged into `event_tx`, on `mcp_manager`'s servers
    /// (a new manager when `None`).
    async fn build_session(
        &self,
        session_id: &str,
        settings: &SessionSettings,
        history: Vec<sven_model::Message>,
        event_tx: mpsc::Sender<AgentEvent>,
        mcp_manager: Option<Arc<sven_bootstrap::McpManager>>,
    ) -> AcpResult<KernelAgentSession> {
        let requester = Arc::new(AcpPermissionRequester {
            session_id: session_id.to_string(),
            conn_tx: self.conn_tx.clone(),
            timeout: self.permission_timeout,
        });
        let mut runtime_ctx = RuntimeContext::auto_detect();
        runtime_ctx.project_root = Some(settings.cwd.clone());
        let mut builder = RuntimeBuilder::new(Arc::clone(&self.config), "agent")
            .with_runtime_context(runtime_ctx)
            .with_model_config(settings.model.clone())
            .with_agent_mode(settings.mode)
            .with_approval_mode(self.approval)
            .with_initial_history(history);
        if let Some(manager) = mcp_manager {
            builder = builder.with_mcp_manager(manager);
        }
        // Under manual approval the kernel asks about every call that is not
        // read-only, `Ask` tools included; asking the client again from the
        // registry would put the same call to it twice.
        if self.approval == ApprovalMode::Auto {
            builder = builder.with_permission_requester(requester.clone());
        }
        let bundle = builder.build_session().await.map_err(|e| {
            tracing::error!("ACP kernel build error: {e:#}");
            Error::internal_error()
        })?;
        let (session, _mcp_event_rx) =
            KernelAgentSession::spawn_answering(bundle, event_tx, gate_responder(requester));
        Ok(session)
    }

    /// Replaces `entry`'s session with one built from `settings`, carrying
    /// the conversation so far and the MCP servers. `session` is the entry's
    /// locked session; the caller has made sure no turn is running on it.
    async fn rebuild_session(
        &self,
        session_id: &str,
        entry: &SessionEntry,
        session: &mut KernelAgentSession,
        settings: SessionSettings,
    ) -> AcpResult<()> {
        let history = session.history_snapshot();
        *session = self
            .build_session(
                session_id,
                &settings,
                history,
                entry.event_tx.clone(),
                Some(Arc::clone(&entry.mcp_manager)),
            )
            .await?;
        *entry
            .settings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = settings;
        entry.model_changed.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Records what a turn's `event` changes about the session's settings:
    /// a model the agent switched to, which the session is rebuilt on before
    /// the next turn.
    fn note_turn_event(&self, entry: &SessionEntry, event: &AgentEvent) {
        if let AgentEvent::ModelChanged(model) = event {
            let model = sven_model::resolve_model_from_config(&self.config, model);
            entry
                .settings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .model = model;
            entry.model_changed.store(true, Ordering::SeqCst);
        }
    }
}

/// Answers a session's kernel gates. ACP carries no question from the agent
/// to the client, so nobody can answer one: it is answered at once, saying
/// so. An approval (manual only) goes to the client with the call's
/// capability, bounded by the permission timeout.
fn gate_responder(
    requester: Arc<AcpPermissionRequester>,
) -> sven_bootstrap::session_handles::HumanGateResponder {
    Arc::new(move |gate| match gate {
        HumanGate::Question { reply_tx, .. } => {
            let _ = reply_tx.send(sven_tool_api::NO_USER_ANSWER.to_string());
        }
        HumanGate::Approval {
            capability,
            call,
            reply_tx,
            ..
        } => {
            let requester = Arc::clone(&requester);
            tokio::spawn(async move {
                let approved = match call {
                    Some(call) => {
                        let call = sven_tool_api::ToolCall {
                            id: uuid::Uuid::new_v4().to_string(),
                            name: call.name,
                            args: call.args,
                        };
                        requester.ask(&call, capability).await
                    }
                    None => false,
                };
                let _ = reply_tx.send(approved);
            });
        }
    })
}

impl SvenAcpAgent {
    /// How long a tool call waits for the client's permission answer before
    /// it is denied.
    #[must_use]
    pub fn with_permission_timeout(mut self, timeout: Duration) -> Self {
        self.permission_timeout = timeout;
        self
    }

    /// Clone the session `Arc` out of the `RefCell` without holding a borrow
    /// across an `.await` point.
    fn get_session(&self, session_id: &str) -> Option<Arc<SessionEntry>> {
        self.sessions.borrow().get(session_id).cloned()
    }

    /// Send one `session/update` notification to the client via the background
    /// task.  Waits for the notification to be dispatched, but gives up after
    /// [`NOTIFY_ACK_TIMEOUT`] to avoid stalling `prompt()` when the IDE stops
    /// draining stdout.
    async fn send_notification(&self, notification: SessionNotification) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self
            .conn_tx
            .send(ConnMessage::SessionUpdate(notification, ack_tx))
            .is_ok()
        {
            let _ = tokio::time::timeout(NOTIFY_ACK_TIMEOUT, ack_rx).await;
        }
    }

    /// Build the list of advertised [`SessionMode`]s.
    fn advertised_modes() -> Vec<SessionMode> {
        vec![
            SessionMode::new(SessionModeId::new("agent"), "Agent").description(
                "Full agentic mode: reads, writes, executes tools autonomously".to_string(),
            ),
            SessionMode::new(SessionModeId::new("plan"), "Plan")
                .description("Planning mode: proposes changes without writing files".to_string()),
            SessionMode::new(SessionModeId::new("research"), "Research")
                .description("Research mode: reads and searches, no file writes".to_string()),
        ]
    }
}

// ─── ACP Agent trait implementation ──────────────────────────────────────────

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Agent for SvenAcpAgent {
    async fn initialize(&self, args: InitializeRequest) -> AcpResult<InitializeResponse> {
        debug!(
            "ACP initialize: protocol_version={:?}",
            args.protocol_version
        );
        let caps = AgentCapabilities::new()
            .prompt_capabilities(PromptCapabilities::new().embedded_context(true));
        Ok(InitializeResponse::new(args.protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                agent_client_protocol::Implementation::new("sven", SVEN_VERSION)
                    .title("Sven AI Coding Agent".to_string()),
            ))
    }

    async fn authenticate(&self, _args: AuthenticateRequest) -> AcpResult<AuthenticateResponse> {
        Ok(AuthenticateResponse::new())
    }

    async fn new_session(&self, args: NewSessionRequest) -> AcpResult<NewSessionResponse> {
        debug!("ACP new_session: cwd={:?}", args.cwd);

        let session_id = uuid::Uuid::new_v4().to_string();
        let settings = SessionSettings {
            cwd: args.cwd.clone(),
            mode: AgentMode::Agent,
            model: self.config.model.clone(),
        };

        // The event receiver is drained one turn at a time by `prompt`; every
        // build of the session (a mode change rebuilds it) sends into it.
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);
        let session = self
            .build_session(&session_id, &settings, Vec::new(), event_tx.clone(), None)
            .await?;

        let initial_mode = settings.mode;
        let entry = Arc::new(SessionEntry {
            mcp_manager: session.mcp_manager(),
            session: tokio::sync::Mutex::new(session),
            settings: std::sync::Mutex::new(settings),
            model_changed: AtomicBool::new(false),
            turns_in_flight: AtomicUsize::new(0),
            event_tx,
            event_rx: tokio::sync::Mutex::new(event_rx),
            cancel_tx: tokio::sync::Mutex::new(None),
        });

        self.sessions.borrow_mut().insert(session_id.clone(), entry);

        let mode_state = SessionModeState::new(
            sven_mode_to_acp_mode_id(initial_mode),
            Self::advertised_modes(),
        );

        Ok(NewSessionResponse::new(session_id).modes(mode_state))
    }

    async fn prompt(&self, args: PromptRequest) -> AcpResult<PromptResponse> {
        let session_id = args.session_id.to_string();
        debug!("ACP prompt: session={session_id}");

        let entry = self
            .get_session(&session_id)
            .ok_or_else(Error::invalid_params)?;

        // Extract text content from the prompt.  Non-text blocks (images, audio,
        // embedded resources) are logged and skipped; we don't yet advertise
        // image/audio prompt capabilities to the IDE.
        let text = args
            .prompt
            .into_iter()
            .filter_map(|block| match block {
                agent_client_protocol::ContentBlock::Text(t) => Some(t.text),
                other => {
                    debug!(
                        session = %session_id,
                        "ACP prompt: dropping non-text ContentBlock variant (not yet supported)"
                    );
                    let _ = other;
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Counted before the first await, so a mode change that arrives while
        // this turn waits or runs sees it and is refused.
        let _turn = entry.begin_turn();

        // Set up cancellation.
        let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
        *entry.cancel_tx.lock().await = Some(cancel_tx);

        // Take exclusive ownership of this session's event stream for the turn.
        let mut event_rx = entry.event_rx.lock().await;

        // Discard any tail events buffered from a previously cancelled or
        // errored turn (e.g. a stray `Aborted` the kernel emits on cancel) so
        // they never bleed into this prompt. A normal turn consumes its own
        // `TurnComplete`, leaving the stream empty here, so this is a no-op
        // in the common case.
        while event_rx.try_recv().is_ok() {}

        // Post the user message to the kernel, on the model the agent last
        // switched to.
        {
            let mut session = entry.session.lock().await;
            if entry.model_changed.load(Ordering::SeqCst) {
                self.rebuild_session(&session_id, &entry, &mut session, entry.settings())
                    .await?;
            }
            session.send_user_message(text).await;
        }

        // Bridge AgentEvents to ACP session/update notifications until the turn
        // completes (TurnComplete), is aborted (Aborted), or errors.
        let mut stop_reason = None::<StopReason>;
        let mut agent_error = None::<String>;
        // What the turn used, reported with its response so a client that
        // pays for this agent (a `task` parent) can charge it.
        let mut used = TurnUsage::default();

        // Pin the oneshot receiver so it can be polled repeatedly in select!
        // without being moved on the first iteration.
        tokio::pin!(cancel_rx);

        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    // Cancellation requested from `cancel()`.
                    entry.session.lock().await.cancel().await;
                    stop_reason = Some(StopReason::Cancelled);
                    break;
                }
                maybe_ev = event_rx.recv() => {
                    let ev = match maybe_ev {
                        Some(ev) => ev,
                        None => break,
                    };

                    match ev {
                        AgentEvent::Error(msg) => {
                            agent_error = Some(msg);
                            break;
                        }
                        AgentEvent::TurnComplete => {
                            stop_reason = Some(StopReason::EndTurn);
                            break;
                        }
                        AgentEvent::Aborted { .. } => {
                            // A user-initiated abort (Esc, Ctrl+C, /abort, or
                            // the thinking watchdog) is terminal on its own -
                            // the kernel no longer follows it with a
                            // `TurnComplete`, so this must break the loop
                            // itself or a cancelled turn would hang forever
                            // waiting for an event that will never arrive.
                            stop_reason = Some(StopReason::Cancelled);
                            break;
                        }
                        other => {
                            used.add(&other);
                            self.note_turn_event(&entry, &other);
                            // Non-terminal events are forwarded when they
                            // have an ACP equivalent; the loop keeps draining
                            // until a terminal `TurnComplete`/`Aborted`/
                            // `Error` arrives.
                            if let Some(update) = agent_event_to_session_update(&other) {
                                let notification =
                                    SessionNotification::new(args.session_id.clone(), update);
                                self.send_notification(notification).await;
                            }
                        }
                    }
                }
            }
        }

        if let Some(msg) = agent_error {
            warn!(session = %session_id, error = %msg, "ACP prompt: kernel reported error");
            return Err(Error::new(i32::from(ErrorCode::InternalError), msg));
        }

        Ok(PromptResponse::new(stop_reason.unwrap_or(StopReason::EndTurn)).usage(used.into_acp()))
    }

    async fn cancel(&self, args: CancelNotification) -> AcpResult<()> {
        let session_id = args.session_id.to_string();
        debug!("ACP cancel: session={session_id}");

        if let Some(entry) = self.get_session(&session_id) {
            let mut guard = entry.cancel_tx.lock().await;
            if let Some(tx) = guard.take() {
                let _ = tx.send(());
            }
        }
        Ok(())
    }

    async fn set_session_mode(
        &self,
        args: SetSessionModeRequest,
    ) -> AcpResult<SetSessionModeResponse> {
        let session_id = args.session_id.to_string();
        debug!(
            "ACP set_session_mode: session={session_id} mode={:?}",
            args.mode_id
        );

        let entry = self
            .get_session(&session_id)
            .ok_or_else(Error::invalid_params)?;

        // The mode decides the kernel's policy and the session's tools, both
        // fixed when a session is built: switching rebuilds it with the
        // session's settings, carrying the conversation so far. Never under
        // a running turn, which would be left waiting on the old session.
        let mut session = entry.session.lock().await;
        if entry.turns_in_flight.load(Ordering::SeqCst) > 0 {
            return Err(Error::new(
                i32::from(ErrorCode::InvalidRequest),
                "the session mode cannot change while a prompt turn is running: \
                 wait for the turn to end, or cancel it, then set the mode",
            ));
        }
        let settings = SessionSettings {
            mode: acp_mode_id_to_sven_mode(&args.mode_id),
            ..entry.settings()
        };
        self.rebuild_session(&session_id, &entry, &mut session, settings)
            .await?;

        Ok(SetSessionModeResponse::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u32, output: u32) -> AgentEvent {
        AgentEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
        }
    }

    #[test]
    fn a_turn_reports_the_tokens_it_used() {
        let mut used = TurnUsage::default();
        used.add(&usage(100, 20));
        used.add(&AgentEvent::TurnComplete);
        used.add(&usage(50, 5));
        let acp = used.into_acp();
        assert_eq!(
            (acp.input_tokens, acp.output_tokens, acp.total_tokens),
            (150, 25, 175)
        );
    }

    async fn ask(timeout: Duration) -> Option<bool> {
        use sven_tool_api::PermissionRequester as _;
        let (conn_tx, mut conn_rx) = mpsc::unbounded_channel();
        let requester = AcpPermissionRequester {
            session_id: "s".into(),
            conn_tx,
            timeout,
        };
        let call = sven_tool_api::ToolCall {
            id: "c".into(),
            name: "shell".into(),
            args: serde_json::json!({"command": "ls"}),
        };
        let asking = requester.request_permission(&call, ToolCapability::ExecuteShell);
        tokio::pin!(asking);
        // Nobody answers; the pending request is held open meanwhile.
        let held = tokio::select! {
            msg = conn_rx.recv() => msg,
            _ = &mut asking => panic!("answered before the request was sent"),
        };
        let Some(ConnMessage::RequestPermission { request, .. }) = &held else {
            panic!("the client is asked");
        };
        let meta = request
            .tool_call
            .meta
            .as_ref()
            .expect("the capability is named");
        assert_eq!(meta[CAPABILITY_META_KEY], "ExecuteShell");
        tokio::time::timeout(Duration::from_secs(5), asking)
            .await
            .ok()
    }

    /// A mode is not a label: switching an ACP session to research rebuilds
    /// it with the research tools and policy, so a sub-agent started in
    /// research mode cannot write.
    #[tokio::test]
    async fn a_mode_switch_rebuilds_the_session_in_that_mode() {
        use agent_client_protocol::Agent as _;
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        let (conn_tx, _conn_rx) = mpsc::unbounded_channel();
        let agent = SvenAcpAgent::new(Arc::new(config), conn_tx);
        let dir = tempfile::tempdir().unwrap();
        let id = agent
            .new_session(NewSessionRequest::new(dir.path()))
            .await
            .expect("a session")
            .session_id
            .to_string();
        let offers = |tool: &'static str| {
            let entry = agent.get_session(&id).expect("the session");
            async move {
                let session = entry.session.lock().await;
                session.tool_registry().get(tool).is_some()
            }
        };
        assert!(offers("write_file").await, "an agent session writes");
        agent
            .set_session_mode(SetSessionModeRequest::new(
                id.clone(),
                SessionModeId::new("research"),
            ))
            .await
            .expect("the mode switches");
        assert!(!offers("write_file").await, "a research session does not");
        assert!(offers("read_file").await);
    }

    fn mock_agent() -> SvenAcpAgent {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        let (conn_tx, _conn_rx) = mpsc::unbounded_channel();
        SvenAcpAgent::new(Arc::new(config), conn_tx)
    }

    /// A mode change while a turn runs would pull the session out from under
    /// it: it is refused, telling the client why, and the turn completes.
    #[tokio::test]
    async fn a_mode_change_during_a_turn_is_refused() {
        use agent_client_protocol::Agent as _;
        let agent = mock_agent();
        let dir = tempfile::tempdir().unwrap();
        let id = agent
            .new_session(NewSessionRequest::new(dir.path()))
            .await
            .expect("a session")
            .session_id;
        let set_mode =
            || agent.set_session_mode(SetSessionModeRequest::new(id.clone(), "research"));
        let (turn, during) = tokio::join!(
            tokio::time::timeout(
                Duration::from_secs(30),
                agent.prompt(PromptRequest::new(id.clone(), vec!["hello".into()])),
            ),
            set_mode(),
        );
        let refused = during.expect_err("refused while the turn runs");
        assert!(refused.message.contains("turn is running"), "{refused:?}");
        let turn = turn
            .expect("the turn is not stranded")
            .expect("the turn ends");
        assert_eq!(turn.stop_reason, StopReason::EndTurn);
        set_mode().await.expect("allowed once the turn is over");
    }

    /// A mode change rebuilds the session with everything the old one was
    /// set up with: its directory, and the model the agent switched to.
    #[tokio::test]
    async fn a_mode_change_keeps_the_session_settings() {
        use agent_client_protocol::Agent as _;
        let agent = mock_agent();
        let dir = tempfile::tempdir().unwrap();
        let id = agent
            .new_session(NewSessionRequest::new(dir.path()))
            .await
            .expect("a session")
            .session_id
            .to_string();
        let entry = agent.get_session(&id).expect("the session");
        agent.note_turn_event(
            &entry,
            &AgentEvent::ModelChanged("mock/second-model".into()),
        );
        agent
            .set_session_mode(SetSessionModeRequest::new(id.clone(), "research"))
            .await
            .expect("the mode switches");
        let settings = entry.settings();
        assert_eq!(settings.mode, AgentMode::Research);
        assert_eq!(settings.cwd, dir.path());
        assert_eq!(
            (
                settings.model.provider.as_str(),
                settings.model.name.as_str()
            ),
            ("mock", "second-model")
        );
    }

    /// Under manual approval a kernel approval goes to the client as a
    /// permission request naming the call's capability; a question gets the
    /// no-user answer.
    #[tokio::test]
    async fn a_kernel_approval_goes_to_the_client_with_its_capability() {
        let (conn_tx, mut conn_rx) = mpsc::unbounded_channel();
        let respond = gate_responder(Arc::new(AcpPermissionRequester {
            session_id: "s".into(),
            conn_tx,
            timeout: Duration::from_secs(5),
        }));
        let (reply_tx, reply_rx) = oneshot::channel();
        respond(HumanGate::Approval {
            capability: ToolCapability::NetworkAccess,
            prompt: "fetch".into(),
            call: Some(sven_hsm::GatedCall {
                name: "web_fetch".into(),
                args: serde_json::json!({"url": "https://example.com"}),
            }),
            reply_tx,
        });
        let Some(ConnMessage::RequestPermission {
            request,
            response_tx,
        }) = conn_rx.recv().await
        else {
            panic!("the client is asked");
        };
        let meta = request.tool_call.meta.expect("the capability is named");
        assert_eq!(meta[CAPABILITY_META_KEY], "NetworkAccess");
        let allow = request.options[0].option_id.clone();
        let _ = response_tx.send(RequestPermissionResponse::new(
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(allow)),
        ));
        assert!(reply_rx.await.unwrap(), "the client allowed it");

        let (reply_tx, reply_rx) = oneshot::channel();
        respond(HumanGate::Question {
            prompt: "which?".into(),
            reply_tx,
        });
        assert_eq!(reply_rx.await.unwrap(), sven_tool_api::NO_USER_ANSWER);
    }

    /// A client that never answers a permission request cannot stall the
    /// session: the call is denied once the wait runs out. The request names
    /// the call's capability, as every request from a tool's policy does.
    #[tokio::test]
    async fn an_unanswered_permission_request_is_denied_when_its_wait_runs_out() {
        assert_eq!(ask(Duration::from_millis(20)).await, Some(false));
    }
}
