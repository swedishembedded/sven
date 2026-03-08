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
    InitializeRequest, InitializeResponse, NewSessionRequest, NewSessionResponse, PromptRequest,
    PromptResponse, Result as AcpResult, SessionMode, SessionModeId, SessionModeState,
    SessionNotification, SetSessionModeRequest, SetSessionModeResponse, StopReason,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as WsMessage},
    Connector,
};
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
            let _ = ack_rx.await;
        }
    }

    fn advertised_modes() -> Vec<SessionMode> {
        vec![
            SessionMode::new(SessionModeId::new("agent"), "Agent"),
            SessionMode::new(SessionModeId::new("plan"), "Plan"),
            SessionMode::new(SessionModeId::new("research"), "Research"),
        ]
    }

    /// Open an authenticated WebSocket connection to the node.
    async fn connect_ws(
        &self,
    ) -> AcpResult<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    > {
        let mut req = self
            .ws_url
            .as_str()
            .into_client_request()
            .map_err(|_| Error::internal_error())?;

        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", self.token).parse().map_err(
                |_: tokio_tungstenite::tungstenite::http::header::InvalidHeaderValue| {
                    Error::internal_error()
                },
            )?,
        );

        // Accept self-signed certs (node uses a local CA by default).
        let tls_cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAny))
            .with_no_client_auth();
        let connector = Connector::Rustls(Arc::new(tls_cfg));

        let (ws, _) = connect_async_tls_with_config(req, None, false, Some(connector))
            .await
            .map_err(|_| Error::internal_error())?;

        Ok(ws)
    }

    async fn send_ws_command(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        cmd: &WsCommand,
    ) -> AcpResult<()> {
        let json = serde_json::to_string(cmd).map_err(|_| Error::internal_error())?;
        ws.send(WsMessage::Text(json))
            .await
            .map_err(|_| Error::internal_error())
    }
}

// ─── ACP Agent trait implementation ──────────────────────────────────────────

#[async_trait::async_trait(?Send)]
impl agent_client_protocol::Agent for SvenAcpNodeProxy {
    async fn initialize(&self, args: InitializeRequest) -> AcpResult<InitializeResponse> {
        debug!("ACP node-proxy initialize");
        Ok(InitializeResponse::new(args.protocol_version)
            .agent_capabilities(AgentCapabilities::new())
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
                                    // Auto-approve in proxy mode; permission requests would require
                                    // bidirectional signalling that is not yet wired.
                                    warn!("ACP proxy: auto-approving tool {tool_name} ({call_id}) args={tool_args}");
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
        _args: SetSessionModeRequest,
    ) -> AcpResult<SetSessionModeResponse> {
        // Mode switching is not forwarded to the node in this version.
        Ok(SetSessionModeResponse::new())
    }
}

// ─── TLS: Accept-all verifier (node uses local CA) ────────────────────────────

#[derive(Debug)]
struct AcceptAny;

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer,
        _intermediates: &[rustls::pki_types::CertificateDer],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer,
        dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dh_params,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer,
        dh_params: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dh_params,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
