// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`SvenAcpAgent`] - implements the ACP `Agent` trait for sven.
//!
//! Each `new_session` call builds a fresh kernel session via
//! [`sven_bootstrap::RuntimeBuilder`] and stores its [`RuntimeHandle`] in a
//! [`SessionEntry`] keyed by ACP [`SessionId`].  `prompt` posts
//! `Event::UserMessage` to the kernel, subscribes to the [`UiEvent`]
//! observation bus, and bridges those events to ACP `session/update`
//! notifications, returning when the turn completes or is cancelled.
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

/// How long to wait for the IDE to respond to a `session/request_permission`
/// request before defaulting to denial.
const PERMISSION_TIMEOUT: Duration = Duration::from_secs(60);

use sven_bootstrap::{RuntimeBuilder, RuntimeContext, RuntimeHandle};
use sven_config::{AgentMode, Config};
use sven_hsm::UiEvent;

use crate::bridge::{
    acp_mode_id_to_sven_mode, sven_mode_to_acp_mode_id, ui_event_to_session_update,
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

/// Implements [`sven_tools::PermissionRequester`] by forwarding permission
/// requests to the IDE over ACP via the `session/request_permission` method.
///
/// Created per session in [`SvenAcpAgent::new_session`] and passed to
/// [`RuntimeBuilder::with_permission_requester`] so that tools with
/// `ApprovalPolicy::Ask` gate their execution on an explicit IDE approval.
struct AcpPermissionRequester {
    session_id: String,
    conn_tx: mpsc::UnboundedSender<ConnMessage>,
}

#[async_trait::async_trait]
impl sven_tools::PermissionRequester for AcpPermissionRequester {
    async fn request_permission(&self, call: &sven_tools::ToolCall) -> bool {
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

        match tokio::time::timeout(PERMISSION_TIMEOUT, response_rx).await {
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
                    PERMISSION_TIMEOUT.as_secs()
                );
                false
            }
        }
    }
}

// ─── Session entry ────────────────────────────────────────────────────────────

/// Per-session state stored inside [`SvenAcpAgent`].
struct SessionEntry {
    /// Kernel handle for posting events into the session runtime.
    handle: RuntimeHandle,
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
}

impl SvenAcpAgent {
    pub fn new(config: Arc<Config>, conn_tx: mpsc::UnboundedSender<ConnMessage>) -> Self {
        Self {
            config,
            sessions: RefCell::new(HashMap::new()),
            conn_tx,
        }
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

        let handle = bundle.handle.clone();

        // Auto-consume kernel-level approval/question channels; tool-level
        // approvals are handled by `AcpPermissionRequester` above.
        let mut channels = bundle.channels;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    q = channels.question_rx.recv() => match q {
                        Some(q) => { let _ = q.reply_tx.send(String::new()); }
                        None => break,
                    },
                    a = channels.approval_rx.recv() => match a {
                        Some(a) => { let _ = a.reply_tx.send(true); }
                        None => break,
                    },
                }
            }
        });

        drop(bundle.runtime);

        let mode_lock = Arc::new(tokio::sync::Mutex::new(initial_mode));

        let entry = Arc::new(SessionEntry {
            handle,
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

        // Subscribe to the kernel observation bus before posting the message
        // so we don't miss the first event.
        let mut obs_rx = entry.handle.subscribe_observations();

        // Post the user message to the kernel.
        entry.handle.send_user_message(text).await;

        // Bridge UiEvents to ACP session/update notifications until the turn
        // completes (TurnComplete), is aborted (UserCancelled), or errors.
        let mut stop_reason = None::<StopReason>;
        let mut agent_error = None::<String>;

        // Pin the oneshot receiver so it can be polled repeatedly in select!
        // without being moved on the first iteration.
        tokio::pin!(cancel_rx);

        loop {
            tokio::select! {
                _ = &mut cancel_rx => {
                    // Cancellation requested from `cancel()`.
                    entry.handle.cancel().await;
                    stop_reason = Some(StopReason::Cancelled);
                    break;
                }
                result = obs_rx.recv() => {
                    use tokio::sync::broadcast::error::RecvError;
                    let ev = match result {
                        Ok(ev) => ev,
                        Err(RecvError::Lagged(_)) => continue,
                        Err(RecvError::Closed) => break,
                    };

                    let is_turn_complete = matches!(ev, UiEvent::TurnComplete);
                    let is_error = matches!(ev, UiEvent::Error(_));

                    if is_error {
                        if let UiEvent::Error(msg) = ev {
                            agent_error = Some(msg);
                        }
                        break;
                    }

                    if let Some(update) = ui_event_to_session_update(&ev) {
                        let notification =
                            SessionNotification::new(args.session_id.clone(), update);
                        self.send_notification(notification).await;
                    }

                    if is_turn_complete {
                        stop_reason = Some(StopReason::EndTurn);
                        break;
                    }
                }
            }
        }

        if let Some(msg) = agent_error {
            warn!(session = %session_id, error = %msg, "ACP prompt: kernel reported error");
            return Err(Error::new(i32::from(ErrorCode::InternalError), msg));
        }

        Ok(PromptResponse::new(stop_reason.unwrap_or(StopReason::EndTurn)))
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
