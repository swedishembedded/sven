// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`SvenAcpNodeProxy`] — proxies ACP requests to a running `sven node` over WebSocket.
//!
//! Instead of building a local agent, every ACP method is translated into the
//! corresponding [`ControlCommand`] and forwarded to a `sven node`.  Events
//! flowing back from the node are translated into ACP `session/update`
//! notifications.
//!
//! This mirrors the pattern in `sven-mcp/src/node_proxy.rs` but operates at
//! the session level rather than the single-tool-call level.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::{
    AgentCapabilities, AuthenticateRequest, AuthenticateResponse, CancelNotification, Error,
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse,
    PromptCapabilities, PromptRequest, PromptResponse, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, Result as AcpResult,
    SelectedPermissionOutcome, SessionMode, SessionModeId, SessionModeState, SessionNotification,
    SetSessionModeRequest, SetSessionModeResponse, StopReason, ToolCallUpdate,
    ToolCallUpdateFields,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::agent::ConnMessage;
use crate::bridge::sven_mode_to_acp_mode_id;

// ─── Wire types (mirrors sven-node control protocol) ──────────────────────────

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsCommand {
    NewSession {
        id: Uuid,
        mode: String,
        working_dir: Option<String>,
    },
    SendInput {
        session_id: Uuid,
        text: String,
    },
    CancelSession {
        session_id: Uuid,
    },
    Subscribe {
        session_id: Uuid,
    },
    SetMode {
        session_id: Uuid,
        mode: String,
    },
    /// Approve or reject a pending tool call that the node flagged as needing approval.
    ApproveToolCall {
        session_id: Uuid,
        call_id: String,
        approved: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsEvent {
    OutputDelta {
        delta: String,
        role: String,
    },
    OutputComplete {},
    ToolCall {
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
    ToolNeedsApproval {
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    SessionState {
        state: NodeSessionState,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum NodeSessionState {
    Idle,
    Running,
    Completed,
    Failed,
    Cancelled,
}

// ─── Node session tracking ─────────────────────────────────────────────────────

struct ProxySession {
    /// The UUID used on the node side (not the ACP session string ID).
    node_session_id: Uuid,
    /// Cancellation channel — fires when the client sends `session/cancel`.
    cancel_tx: tokio::sync::Mutex<Option<oneshot::Sender<()>>>,
}

// ─── SvenAcpNodeProxy ─────────────────────────────────────────────────────────

const SVEN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Notification ack timeout — mirrors the same constant in `agent.rs`.
const NOTIFY_ACK_TIMEOUT: Duration = Duration::from_secs(30);

/// ACP agent implementation that proxies all requests to a running `sven node`.
///
/// `!Send` due to `RefCell`.
pub struct SvenAcpNodeProxy {
    ws_url: String,
    token: String,
    sessions: RefCell<HashMap<String, Arc<ProxySession>>>,
    conn_tx: mpsc::UnboundedSender<ConnMessage>,
}

impl SvenAcpNodeProxy {
    pub fn new(ws_url: String, token: String, conn_tx: mpsc::UnboundedSender<ConnMessage>) -> Self {
        Self {
            ws_url,
            token,
            sessions: RefCell::new(HashMap::new()),
            conn_tx,
        }
    }

    fn get_session(&self, id: &str) -> Option<Arc<ProxySession>> {
        self.sessions.borrow().get(id).cloned()
    }

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

    fn advertised_modes() -> Vec<SessionMode> {
        vec![
            SessionMode::new(SessionModeId::new("agent"), "Agent"),
            SessionMode::new(SessionModeId::new("plan"), "Plan"),
            SessionMode::new(SessionModeId::new("research"), "Research"),
        ]
    }

    async fn connect_ws(&self) -> AcpResult<sven_node_client::NodeWsStream> {
        sven_node_client::connect(&self.ws_url, &self.token)
            .await
            .map_err(|_| Error::internal_error())
    }

    async fn send_ws_command(
        ws: &mut sven_node_client::NodeWsStream,
        cmd: &WsCommand,
    ) -> AcpResult<()> {
        sven_node_client::send_json(ws, cmd)
            .await
            .map_err(|_| Error::internal_error())
    }
}

// ─── ACP Agent trait implementation ──────────────────────────────────────────

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Agent for SvenAcpNodeProxy {
    async fn initialize(&self, args: InitializeRequest) -> AcpResult<InitializeResponse> {
        debug!("ACP node-proxy initialize");
        let caps = AgentCapabilities::new()
            .prompt_capabilities(PromptCapabilities::new().embedded_context(true));
        Ok(InitializeResponse::new(args.protocol_version)
            .agent_capabilities(caps)
            .agent_info(
                agent_client_protocol::Implementation::new("sven-node-proxy", SVEN_VERSION)
                    .title("Sven Node Proxy".to_string()),
            ))
    }

    async fn authenticate(&self, _args: AuthenticateRequest) -> AcpResult<AuthenticateResponse> {
        Ok(AuthenticateResponse::new())
    }

    async fn new_session(&self, args: NewSessionRequest) -> AcpResult<NewSessionResponse> {
        debug!("ACP node-proxy new_session");

        let acp_session_id = uuid::Uuid::new_v4().to_string();
        let node_session_id = Uuid::new_v4();

        let mut ws = self.connect_ws().await?;

        Self::send_ws_command(
            &mut ws,
            &WsCommand::NewSession {
                id: node_session_id,
                mode: "agent".to_string(),
                working_dir: Some(args.cwd.to_string_lossy().into_owned()),
            },
        )
        .await?;

        // Wait for SessionState::Idle (session created)
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(WsMessage::Text(text)) => {
                    if let Ok(WsEvent::SessionState { state, .. }) =
                        serde_json::from_str::<WsEvent>(&text)
                    {
                        if state == NodeSessionState::Idle {
                            break;
                        }
                    }
                }
                Err(_e) => {
                    return Err(Error::internal_error());
                }
                _ => {}
            }
        }

        let session = Arc::new(ProxySession {
            node_session_id,
            cancel_tx: tokio::sync::Mutex::new(None),
        });

        self.sessions
            .borrow_mut()
            .insert(acp_session_id.clone(), session);

        let mode_state = SessionModeState::new(
            sven_mode_to_acp_mode_id(sven_config::AgentMode::Agent),
            Self::advertised_modes(),
        );

        Ok(NewSessionResponse::new(acp_session_id).modes(mode_state))
    }

    async fn prompt(&self, args: PromptRequest) -> AcpResult<PromptResponse> {
        let acp_session_id = args.session_id.to_string();
        debug!("ACP node-proxy prompt: session={acp_session_id}");

        let proxy_session = self
            .get_session(&acp_session_id)
            .ok_or_else(Error::invalid_params)?;

        let text = args
            .prompt
            .into_iter()
            .filter_map(|block| match block {
                agent_client_protocol::ContentBlock::Text(t) => Some(t.text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        *proxy_session.cancel_tx.lock().await = Some(cancel_tx);

        let mut ws = self.connect_ws().await?;

        // Subscribe to events for this session.
        Self::send_ws_command(
            &mut ws,
            &WsCommand::Subscribe {
                session_id: proxy_session.node_session_id,
            },
        )
        .await?;

        // Send the user input.
        Self::send_ws_command(
            &mut ws,
            &WsCommand::SendInput {
                session_id: proxy_session.node_session_id,
                text,
            },
        )
        .await?;

        let stop_reason = loop {
            tokio::select! {
                msg = ws.next() => {
                    match msg {
                        Some(Ok(WsMessage::Text(text))) => {
                            match serde_json::from_str::<WsEvent>(&text) {
                                Ok(WsEvent::OutputDelta { delta, role, .. }) => {
                                    use agent_client_protocol::{ContentBlock, ContentChunk, SessionUpdate};
                                    let update = if role == "thinking" {
                                        SessionUpdate::AgentThoughtChunk(ContentChunk::new(
                                            ContentBlock::from(delta.as_str()),
                                        ))
                                    } else {
                                        SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                            ContentBlock::from(delta.as_str()),
                                        ))
                                    };
                                    let notification = SessionNotification::new(
                                        args.session_id.clone(),
                                        update,
                                    );
                                    self.send_notification(notification).await;
                                }
                                Ok(WsEvent::ToolCall { call_id, tool_name, args: tool_args, .. }) => {
                                    use agent_client_protocol::{SessionUpdate, ToolCall as AcpToolCall, ToolCallStatus, ToolKind};
                                    let acp_tc = AcpToolCall::new(call_id.clone(), tool_name.clone())
                                        .kind(ToolKind::Other)
                                        .status(ToolCallStatus::InProgress)
                                        .raw_input(tool_args);
                                    let notification = SessionNotification::new(
                                        args.session_id.clone(),
                                        SessionUpdate::ToolCall(acp_tc),
                                    );
                                    self.send_notification(notification).await;
                                }
                                Ok(WsEvent::ToolResult { call_id, output, is_error, .. }) => {
                                    use agent_client_protocol::{SessionUpdate, ToolCall as AcpToolCall, ToolCallStatus, ToolKind};
                                    let status = if is_error { ToolCallStatus::Failed } else { ToolCallStatus::Completed };
                                    let acp_tc = AcpToolCall::new(call_id.clone(), call_id.clone())
                                        .kind(ToolKind::Other)
                                        .status(status)
                                        .raw_output(serde_json::Value::String(output));
                                    let notification = SessionNotification::new(
                                        args.session_id.clone(),
                                        SessionUpdate::ToolCall(acp_tc),
                                    );
                                    self.send_notification(notification).await;
                                }
                                Ok(WsEvent::OutputComplete { .. }) => {}
                                Ok(WsEvent::SessionState { state, .. }) => {
                                    match state {
                                        NodeSessionState::Completed => break StopReason::EndTurn,
                                        NodeSessionState::Failed => break StopReason::EndTurn,
                                        NodeSessionState::Cancelled => break StopReason::Cancelled,
                                        _ => {}
                                    }
                                }
                                Ok(WsEvent::ToolNeedsApproval { call_id, tool_name, args: tool_args, .. }) => {
                                    // Forward the approval request to the IDE via ACP
                                    // `session/request_permission`.  If the IDE approves, send
                                    // `ApproveToolCall { approved: true }` back to the node;
                                    // otherwise send `approved: false`.
                                    let tool_call_update = ToolCallUpdate::new(
                                        call_id.clone(),
                                        ToolCallUpdateFields::new()
                                            .title(tool_name.clone())
                                            .raw_input(tool_args),
                                    );
                                    let allow_once_id = "allow_once";
                                    let reject_once_id = "reject_once";
                                    let options = vec![
                                        agent_client_protocol::PermissionOption::new(
                                            allow_once_id,
                                            "Allow once",
                                            agent_client_protocol::PermissionOptionKind::AllowOnce,
                                        ),
                                        agent_client_protocol::PermissionOption::new(
                                            reject_once_id,
                                            "Reject",
                                            agent_client_protocol::PermissionOptionKind::RejectOnce,
                                        ),
                                    ];
                                    let permission_req = RequestPermissionRequest::new(
                                        args.session_id.clone(),
                                        tool_call_update,
                                        options,
                                    );
                                    let (response_tx, response_rx) = oneshot::channel::<RequestPermissionResponse>();
                                    if self.conn_tx.send(ConnMessage::RequestPermission {
                                        request: permission_req,
                                        response_tx,
                                    }).is_ok() {
                                        let approved = match tokio::time::timeout(
                                            Duration::from_secs(60),
                                            response_rx,
                                        ).await {
                                            Ok(Ok(resp)) => matches!(
                                                resp.outcome,
                                                RequestPermissionOutcome::Selected(SelectedPermissionOutcome { ref option_id, .. })
                                                if option_id.0.as_ref() == allow_once_id
                                            ),
                                            _ => false,
                                        };
                                        let approve_cmd = WsCommand::ApproveToolCall {
                                            session_id: proxy_session.node_session_id,
                                            call_id: call_id.clone(),
                                            approved,
                                        };
                                        if let Err(e) = Self::send_ws_command(&mut ws, &approve_cmd).await {
                                            warn!("ACP proxy: failed to send ApproveToolCall: {e}");
                                        }
                                    } else {
                                        warn!("ACP proxy: conn_tx closed, cannot forward permission request for {tool_name}");
                                    }
                                }
                                Ok(WsEvent::Unknown) => {}
                                Err(e) => {
                                    warn!("ACP proxy: invalid event JSON: {e}");
                                }
                            }
                        }
                        Some(Err(e)) => {
                            warn!("ACP proxy WS error: {e}");
                            break StopReason::EndTurn;
                        }
                        None => break StopReason::EndTurn,
                        _ => {}
                    }
                }
                _ = &mut cancel_rx => {
                    let _ = Self::send_ws_command(
                        &mut ws,
                        &WsCommand::CancelSession {
                            session_id: proxy_session.node_session_id,
                        },
                    ).await;
                    break StopReason::Cancelled;
                }
            }
        };

        Ok(PromptResponse::new(stop_reason))
    }

    async fn cancel(&self, args: CancelNotification) -> AcpResult<()> {
        let acp_session_id = args.session_id.to_string();
        if let Some(proxy_session) = self.get_session(&acp_session_id) {
            let mut guard = proxy_session.cancel_tx.lock().await;
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
        let acp_session_id = args.session_id.to_string();
        debug!(
            "ACP node-proxy set_session_mode: session={acp_session_id} mode={:?}",
            args.mode_id
        );

        let proxy_session = match self.get_session(&acp_session_id) {
            Some(s) => s,
            None => return Err(Error::invalid_params()),
        };

        let mut ws = self.connect_ws().await?;
        Self::send_ws_command(
            &mut ws,
            &WsCommand::SetMode {
                session_id: proxy_session.node_session_id,
                mode: args.mode_id.0.to_string(),
            },
        )
        .await?;

        Ok(SetSessionModeResponse::new())
    }
}
