// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Node-proxy agent backend shared by all Sven frontends.
//!
//! When `SVEN_NODE_URL` and `SVEN_NODE_TOKEN` are set, this module replaces
//! the local agent with a thin WebSocket bridge to the running sven node.
//! That agent has a live `P2pHandle`, so all peer tools (`list_peers`,
//! `delegate_task`, `send_message`, ...) are available.
//!
//! # Protocol
//!
//! The bridge speaks the node's JSON-over-WebSocket control protocol
//! ([`sven_control`]):
//!
//! - `AgentRequest::Submit { content }` → `NewSession` + `SendInput`
//! - `ControlEvent::Session { event, .. }` → forwarded verbatim (`AgentEvent`
//!   and the node's `SessionEvent` are the same type)
//! - `ControlEvent::ToolNeedsApproval { ... }` → auto-approve
//! - `ControlEvent::SessionState { Completed | Cancelled }` → `AgentEvent::TurnComplete`
//! - `ControlEvent::NodeError` → `AgentEvent::Error`

use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use sven_machines::AgentEvent;
use sven_tools::ToolSchema;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

use sven_control::{ControlCommand as Cmd, ControlEvent as Evt, SessionState};

use crate::agent::AgentRequest;

// ── Public entry points ────────────────────────────────────────────────────────

/// Background task that bridges a frontend to a running sven node via WebSocket.
///
/// Replaces `kernel_session_task` when `SVEN_NODE_URL` and `SVEN_NODE_TOKEN`
/// are present in the environment.
pub async fn node_agent_task(
    node_url: String,
    node_token: String,
    insecure: bool,
    mut rx: mpsc::Receiver<AgentRequest>,
    tx: mpsc::Sender<AgentEvent>,
    cancel_handle: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
) {
    let ws_stream = match connect_control_ws(&node_url, &node_token, insecure).await {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send(AgentEvent::Error(e.to_string())).await;
            return;
        }
    };

    let (ws_sink, mut ws_stream) = ws_stream.split();

    let (ws_out_tx, mut ws_out_rx) = mpsc::unbounded_channel::<String>();

    tokio::spawn(async move {
        use futures::SinkExt;
        let mut ws_sink = ws_sink;
        while let Some(json) = ws_out_rx.recv().await {
            if ws_sink
                .send(tungstenite::Message::Text(json))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // The model the remote session should run. `/model` in the TUI arrives as a
    // per-message `model_override`; we remember it and (re)apply it as a
    // `SetModel` on every turn's session so a remote `/model` re-points the
    // server-side provider instead of only changing the local UI.
    let mut active_model: Option<String> = None;

    loop {
        let req = match rx.recv().await {
            Some(r) => r,
            None => break,
        };

        let content = match req {
            AgentRequest::Submit {
                content,
                model_override,
                ..
            } => {
                if let Some(cfg) = model_override {
                    active_model = Some(format!("{}/{}", cfg.provider, cfg.name));
                }
                content
            }
            AgentRequest::Resubmit {
                new_user_content: content,
                model_override,
                ..
            } => {
                if let Some(cfg) = model_override {
                    active_model = Some(format!("{}/{}", cfg.provider, cfg.name));
                }
                content
            }
            AgentRequest::LoadHistory(_) => {
                debug!("node_agent_task: ignoring LoadHistory (node manages history)");
                continue;
            }
            AgentRequest::GenerateTitle { .. } => {
                continue;
            }
            AgentRequest::RefreshMcpTools => {
                continue;
            }
            AgentRequest::ShareSession(_) => {
                // One-tap `/share` exposes a *local* in-process kernel; in
                // node-proxy mode the session runs on the remote node, so there
                // is no local RuntimeHandle to bridge.
                let _ = tx
                    .send(AgentEvent::Error(
                        "/share is only available for local sessions, not node-proxy mode".into(),
                    ))
                    .await;
                continue;
            }
            AgentRequest::ListPeers => {
                if send_cmd(&ws_out_tx, &Cmd::ListPeers).is_err() {
                    let _ = tx.send(AgentEvent::Error("WS send failed".into())).await;
                    break;
                }
                let timeout = tokio::time::Duration::from_secs(5);
                let _ = tokio::time::timeout(timeout, async {
                    loop {
                        let msg = match ws_stream.next().await {
                            Some(Ok(m)) => m,
                            _ => break,
                        };
                        let text = match msg {
                            tungstenite::Message::Text(t) => t,
                            tungstenite::Message::Close(_) => break,
                            _ => continue,
                        };
                        let evt: Evt = match serde_json::from_str(&text) {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        let is_peer_list = matches!(evt, Evt::PeerList { .. });
                        handle_event(evt, &tx, &ws_out_tx, Uuid::nil()).await;
                        if is_peer_list {
                            break;
                        }
                    }
                })
                .await
                .ok();
                continue;
            }
        };

        let sid = Uuid::new_v4();
        let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
        *cancel_handle.lock().await = Some(cancel_tx);

        if send_cmd(
            &ws_out_tx,
            &Cmd::NewSession {
                id: sid,
                mode: sven_config::AgentMode::Agent,
                working_dir: None,
            },
        )
        .is_err()
        {
            let _ = tx.send(AgentEvent::Error("WS send failed".into())).await;
            break;
        }

        // Apply the operator's chosen model to this session before the turn.
        // The remote resolves the override against its OWN config/keys and
        // rebuilds the session kernel around the new provider.
        if let Some(model) = &active_model {
            if send_cmd(
                &ws_out_tx,
                &Cmd::SetModel {
                    session_id: sid,
                    model: model.clone(),
                },
            )
            .is_err()
            {
                let _ = tx.send(AgentEvent::Error("WS send failed".into())).await;
                break;
            }
        }

        if send_cmd(
            &ws_out_tx,
            &Cmd::SendInput {
                session_id: sid,
                text: content,
            },
        )
        .is_err()
        {
            let _ = tx.send(AgentEvent::Error("WS send failed".into())).await;
            break;
        }

        // Tracks whether the turn ended with a terminal `AgentEvent` (TurnComplete
        // / Aborted / Error). Every exit that emits one sets this true; any exit
        // that does NOT (a mid-turn stream EOF or WebSocket Close) leaves it false
        // so the guard below can synthesise the missing completion.
        let mut turn_terminated = false;
        loop {
            tokio::select! {
                msg = ws_stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            let _ = tx.send(AgentEvent::Error(format!("WS recv: {e}"))).await;
                            turn_terminated = true;
                            break;
                        }
                        None => break,
                    };
                    let text = match msg {
                        tungstenite::Message::Text(t) => t,
                        tungstenite::Message::Close(_) => break,
                        _ => continue,
                    };
                    let evt: Evt = match serde_json::from_str(&text) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let done = handle_event(evt, &tx, &ws_out_tx, sid).await;
                    if done {
                        turn_terminated = true;
                        break;
                    }
                }
                Ok(()) = &mut cancel_rx => {
                    let _ = send_cmd(&ws_out_tx, &Cmd::CancelSession { session_id: sid });
                    let _ = tx.send(AgentEvent::Aborted { partial_text: String::new() }).await;
                    turn_terminated = true;
                    break;
                }
            }
        }
        // Belt-and-suspenders: a remote turn MUST always end with a terminal
        // `AgentEvent` so the frontend clears its busy flag. If the node dropped
        // the connection mid-turn (stream EOF or WebSocket Close), no
        // SessionState{completed} arrived — without this, `agent.busy` would stay
        // true forever and the TUI's 80ms anim_tick would repaint the screen at
        // 12fps indefinitely (cursor flicker, no text selection). See the
        // idle-must-be-stable note in the sven-tui run loop.
        if let Some(ev) = turn_exit_event(turn_terminated) {
            let _ = tx.send(ev).await;
        }
        cancel_handle.lock().await.take();
    }
}

/// Fetch the list of tools registered on the connected node.
pub async fn fetch_node_tools(url: &str, token: &str, insecure: bool) -> Vec<ToolSchema> {
    let ws_stream = match connect_control_ws(url, token, insecure).await {
        Ok(s) => s,
        Err(_) => return vec![],
    };

    let (mut ws_sink, mut ws_rx) = ws_stream.split();

    let json = match serde_json::to_string(&Cmd::ListTools) {
        Ok(j) => j,
        Err(_) => return vec![],
    };
    {
        use futures::SinkExt;
        if ws_sink
            .send(tungstenite::Message::Text(json))
            .await
            .is_err()
        {
            return vec![];
        }
    }

    let timeout = tokio::time::Duration::from_secs(5);
    let result = tokio::time::timeout(timeout, async {
        while let Some(msg) = ws_rx.next().await {
            let text = match msg {
                Ok(tungstenite::Message::Text(t)) => t,
                Ok(tungstenite::Message::Close(_)) => break,
                _ => continue,
            };
            if let Ok(Evt::ToolList { tools }) = serde_json::from_str::<Evt>(&text) {
                return tools
                    .into_iter()
                    .map(|t| ToolSchema {
                        name: t.name,
                        description: t.description,
                        parameters: t.parameters,
                        is_mcp: false,
                    })
                    .collect::<Vec<_>>();
            }
        }
        vec![]
    })
    .await;

    result.unwrap_or_default()
}

// ── Connection helper ──────────────────────────────────────────────────────────

/// A WebSocket connection to a node/cloud control endpoint.
pub(crate) type ControlWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens an authenticated WebSocket to a control endpoint.
///
/// Shared by the single-session node bridge, the tool-schema fetch, and the
/// operator console ([`crate::operator`]). TLS certificate verification is
/// skipped when `insecure` is set or the URL points at loopback.
pub(crate) async fn connect_control_ws(
    url: &str,
    token: &str,
    insecure: bool,
) -> anyhow::Result<ControlWs> {
    use tokio_tungstenite::connect_async_tls_with_config;
    use tungstenite::http::Request;

    let insecure = insecure || is_localhost_url(url);
    let connector = build_tls_connector(insecure);

    let request = Request::builder()
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Host", host_header(url)?)
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_ws_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())
        .map_err(|e| anyhow::anyhow!("WS request build: {e}"))?;

    let (ws_stream, _) = connect_async_tls_with_config(request, None, false, connector)
        .await
        .map_err(|e| anyhow::anyhow!("Could not connect to node at {url}: {e}"))?;
    Ok(ws_stream)
}

// ── Helpers ────────────────────────────────────────────────────────────────────

async fn handle_event(
    evt: Evt,
    tx: &mpsc::Sender<AgentEvent>,
    ws_out_tx: &mpsc::UnboundedSender<String>,
    session_id: Uuid,
) -> bool {
    match evt {
        // `AgentEvent` and the node's `ControlEvent::Session`-carried
        // `SessionEvent` are the same type now, so this is a direct forward
        // -- no more role-string sniffing to tell text from thinking apart,
        // no more fabricating an empty tool_name for ToolCallFinished (the
        // old flattened `ControlEvent::ToolResult` had dropped it).
        Evt::Session { event, .. } => {
            let terminal = matches!(
                event,
                AgentEvent::TurnComplete | AgentEvent::Aborted { .. }
            );
            let _ = tx.send(event).await;
            if terminal {
                return true;
            }
        }
        Evt::ToolNeedsApproval {
            call_id, tool_name, ..
        } => {
            let approve = Cmd::ApproveTool {
                session_id,
                call_id,
            };
            if send_cmd(ws_out_tx, &approve).is_err() {
                warn!("failed to auto-approve tool {tool_name}");
            }
        }
        Evt::SessionState { state, .. } => {
            if state == SessionState::Completed || state == SessionState::Cancelled {
                let _ = tx.send(AgentEvent::TurnComplete).await;
                return true;
            }
        }
        Evt::NodeError { message, .. } => {
            let _ = tx.send(AgentEvent::Error(message)).await;
            return true;
        }
        Evt::ToolList { .. }
        | Evt::SessionList { .. }
        | Evt::ToolCallOutput { .. }
        | Evt::WebDeviceList { .. }
        | Evt::WebDeviceUpdated { .. }
        | Evt::WebDeviceError { .. }
        | Evt::History { .. }
        | Evt::Unknown => {}
        Evt::PeerList { peers } => {
            let peer_infos = peers
                .into_iter()
                .map(|p| sven_machines::PeerInfo {
                    name: p.name,
                    peer_id: p.peer_id,
                    connected: p.connected,
                    can_delegate: p.can_delegate,
                })
                .collect();
            let _ = tx.send(AgentEvent::PeerList(peer_infos)).await;
        }
    }
    false
}

/// The terminal [`AgentEvent`] a per-turn receive loop must emit on exit, given
/// whether a terminal event was already sent during the turn.
///
/// A remote turn normally ends via `SessionState{completed|cancelled}` →
/// [`AgentEvent::TurnComplete`], an error, or a local cancel → `Aborted`. But
/// the loop can also exit on a mid-turn stream EOF / WebSocket `Close`, which
/// carries no completion. Returning `TurnComplete` in that case guarantees the
/// frontend always clears its busy flag, so a stray busy state can never drive a
/// runaway spinner repaint.
fn turn_exit_event(turn_terminated: bool) -> Option<AgentEvent> {
    (!turn_terminated).then_some(AgentEvent::TurnComplete)
}

pub(crate) fn send_cmd(
    tx: &mpsc::UnboundedSender<String>,
    cmd: &impl Serialize,
) -> anyhow::Result<()> {
    let json = serde_json::to_string(cmd)?;
    tx.send(json)
        .map_err(|_| anyhow::anyhow!("WS writer channel closed"))
}

fn generate_ws_key() -> String {
    use base64::Engine;
    let mut bytes = [0u8; 16];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn is_localhost_url(url: &str) -> bool {
    url.contains("://127.0.0.1:") || url.contains("://localhost:") || url.contains("://[::1]:")
}

/// The `Host` header for a control endpoint, derived from the URL authority.
/// The operator console dials remote cloud tenant endpoints that commonly
/// sit behind name-routing reverse proxies — a hardcoded loopback literal
/// would be routed to the wrong backend (or 404/421) by any such proxy.
fn host_header(url: &str) -> anyhow::Result<String> {
    let uri: tungstenite::http::Uri = url
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid control URL {url:?}: {e}"))?;
    let host = uri
        .host()
        .ok_or_else(|| anyhow::anyhow!("control URL {url:?} has no host"))?;
    // Re-bracket bare IPv6 literals so the Host header's port separator
    // stays unambiguous (http::Uri may hand back either form).
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(match uri.port_u16() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn build_tls_connector(insecure: bool) -> Option<tokio_tungstenite::Connector> {
    if !insecure {
        return None;
    }

    use std::sync::Arc as StdArc;

    use rustls::{
        client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        pki_types::{CertificateDer, ServerName, UnixTime},
        ClientConfig,
    };

    #[derive(Debug)]
    struct AcceptAnyCert;

    impl ServerCertVerifier for AcceptAnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(StdArc::new(AcceptAnyCert))
        .with_no_client_auth();
    Some(tokio_tungstenite::Connector::Rustls(StdArc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A remote `SessionState{completed}` MUST map to `AgentEvent::TurnComplete`
    // (the event that clears the TUI/GUI busy flag) and terminate the turn loop.
    // If this regresses, `agent.busy` never clears in node-proxy mode and the
    // 80ms anim_tick repaints the screen forever.
    #[tokio::test]
    async fn session_state_completed_maps_to_turn_complete() {
        for state in [SessionState::Completed, SessionState::Cancelled] {
            let (tx, mut rx) = mpsc::channel(4);
            let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
            let done = handle_event(
                Evt::SessionState {
                    session_id: Uuid::nil(),
                    state: state.clone(),
                },
                &tx,
                &ws_tx,
                Uuid::nil(),
            )
            .await;
            assert!(done, "state {state:?} should terminate the turn");
            assert!(
                matches!(rx.try_recv(), Ok(AgentEvent::TurnComplete)),
                "state {state:?} should emit TurnComplete",
            );
        }
    }

    // A non-terminal state (e.g. running) must NOT complete the turn.
    #[tokio::test]
    async fn session_state_running_does_not_complete() {
        let (tx, mut rx) = mpsc::channel(4);
        let (ws_tx, _ws_rx) = mpsc::unbounded_channel();
        let done = handle_event(
            Evt::SessionState {
                session_id: Uuid::nil(),
                state: SessionState::Running,
            },
            &tx,
            &ws_tx,
            Uuid::nil(),
        )
        .await;
        assert!(!done);
        assert!(rx.try_recv().is_err());
    }

    // Belt-and-suspenders: when the per-turn loop exits WITHOUT a terminal event
    // (mid-turn disconnect: stream EOF or WebSocket Close), it must still emit
    // TurnComplete so the frontend clears busy and the repaint loop stops.
    #[test]
    fn turn_exit_synthesises_completion_on_disconnect() {
        // Turn already terminated normally: no duplicate completion.
        assert!(turn_exit_event(true).is_none());
        // Turn dropped mid-flight: synthesise TurnComplete so busy clears.
        assert!(matches!(
            turn_exit_event(false),
            Some(AgentEvent::TurnComplete)
        ));
    }

    #[test]
    fn host_header_follows_the_url_authority() {
        assert_eq!(
            host_header("wss://tenant-a.cloud.example.com/ws").unwrap(),
            "tenant-a.cloud.example.com"
        );
        assert_eq!(
            host_header("wss://tenant-a.cloud.example.com:8443/ws").unwrap(),
            "tenant-a.cloud.example.com:8443"
        );
        assert_eq!(host_header("ws://127.0.0.1:9000/ws").unwrap(), "127.0.0.1:9000");
        assert_eq!(host_header("ws://[::1]:9000/ws").unwrap(), "[::1]:9000");
        assert!(host_header("not a url").is_err());
    }
}
