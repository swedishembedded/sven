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
//! The session is stored in a [`SessionEntry`] keyed by ACP [`SessionId`].
//! `prompt` posts the user message through the session, drains the mapped
//! `AgentEvent` stream, and bridges each event to an ACP `session/update`
//! notification, returning when the turn completes or is cancelled.
//!
//! The struct is intentionally `!Send` (it uses `RefCell` for interior
//! mutability) and lives inside a `tokio::task::LocalSet` spawned by
//! [`crate::serve_stdio`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

use sven_bootstrap::{KernelAgentSession, RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, Config};
use sven_machines::AgentEvent;
use sven_tools_agent::QuestionRequest;

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
    async fn request_permission(&self, call: &sven_tool_api::ToolCall) -> bool {
        // Clone all borrowed data up-front so the future is 'static and Send.
        let call_id = call.id.clone();
        let call_name = call.name.clone();
        let call_args = call.args.clone();
        let session_id = self.session_id.clone();
        let conn_tx = self.conn_tx.clone();

        let tool_call_update = ToolCallUpdate::new(
            call_id,
            ToolCallUpdateFields::new()
                .title(call_name.clone())
                .raw_input(call_args),
        );

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

/// Per-session state stored inside [`SvenAcpAgent`].
struct SessionEntry {
    /// Kernel session bridged onto the shared [`AgentEvent`] stream; owns the
    /// runtime and keeps the kernel alive for the session's lifetime.
    session: KernelAgentSession,
    /// Receiver for this session's mapped [`AgentEvent`] stream, drained one
    /// turn at a time by `prompt`.
    event_rx: tokio::sync::Mutex<mpsc::Receiver<AgentEvent>>,
    /// Mode lock shared between the agent loop and mode-change requests.
    mode_lock: Arc<tokio::sync::Mutex<AgentMode>>,
    /// Cancellation sender; replaced on each new prompt turn.
    cancel_tx: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
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
}

impl SvenAcpAgent {
    pub fn new(config: Arc<Config>, conn_tx: mpsc::UnboundedSender<ConnMessage>) -> Self {
        Self {
            config,
            sessions: RefCell::new(HashMap::new()),
            conn_tx,
            permission_timeout: DEFAULT_PERMISSION_TIMEOUT,
        }
    }

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
        let initial_mode = AgentMode::Agent;

        let permission_requester = Arc::new(AcpPermissionRequester {
            session_id: session_id.clone(),
            conn_tx: self.conn_tx.clone(),
            timeout: self.permission_timeout,
        });

        let mut runtime_ctx = RuntimeContext::auto_detect();
        runtime_ctx.project_root = Some(args.cwd.clone());

        let bundle = RuntimeBuilder::new(Arc::clone(&self.config), "agent")
            .with_runtime_context(runtime_ctx)
            .with_permission_requester(permission_requester)
            .build_session()
            .await
            .map_err(|e| {
                tracing::error!("ACP kernel build error: {e:#}");
                Error::internal_error()
            })?;

        // Bridge the kernel session onto the shared `AgentEvent` stream via the
        // reusable `KernelAgentSession` adapter. The event receiver is drained
        // one turn at a time by `prompt`; the question channel carries
        // kernel-level clarification / approval gates.
        let (event_tx, event_rx) = mpsc::channel::<AgentEvent>(256);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(16);
        let (session, _mcp_event_rx) = KernelAgentSession::spawn(bundle, event_tx, question_tx);

        // Auto-answer kernel-level gates so headless sessions never block:
        // free-text questions resolve to an empty answer and capability
        // approvals are granted. Tool-call approvals are gated separately via
        // `AcpPermissionRequester` on the IDE `session/request_permission` path.
        tokio::spawn(async move {
            while let Some(req) = question_rx.recv().await {
                let is_approval = req.questions.first().is_some_and(|q| !q.options.is_empty());
                let answer = if is_approval {
                    "yes".to_string()
                } else {
                    String::new()
                };
                let _ = req.answer_tx.send(answer);
            }
        });

        let mode_lock = Arc::new(tokio::sync::Mutex::new(initial_mode));

        let entry = Arc::new(SessionEntry {
            session,
            event_rx: tokio::sync::Mutex::new(event_rx),
            mode_lock,
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

        // Post the user message to the kernel.
        entry.session.send_user_message(text).await;

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
                    entry.session.cancel().await;
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

        let new_mode = acp_mode_id_to_sven_mode(&args.mode_id);
        *entry.mode_lock.lock().await = new_mode;

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
        let asking = requester.request_permission(&call);
        tokio::pin!(asking);
        // Nobody answers; the pending request is held open meanwhile.
        let _held = tokio::select! {
            msg = conn_rx.recv() => msg,
            _ = &mut asking => panic!("answered before the request was sent"),
        };
        tokio::time::timeout(Duration::from_secs(5), asking)
            .await
            .ok()
    }

    /// A client that never answers a permission request cannot stall the
    /// session: the call is denied once the wait runs out.
    #[tokio::test]
    async fn an_unanswered_permission_request_is_denied_when_its_wait_runs_out() {
        assert_eq!(ask(Duration::from_millis(20)).await, Some(false));
    }
}
