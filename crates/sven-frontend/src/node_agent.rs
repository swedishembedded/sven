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
//! ([`crate::control`]):
//!
//! - `AgentRequest::Submit { content }` → `NewSession` + `SendInput`
//! - `ControlEvent::OutputDelta { role: "assistant" }` → `AgentEvent::TextDelta`
//! - `ControlEvent::OutputDelta { role: "thinking" }` → `AgentEvent::ThinkingDelta`
//! - `ControlEvent::OutputComplete { role: "assistant" }` → `AgentEvent::TextComplete`
//! - `ControlEvent::OutputComplete { role: "thinking" }` → `AgentEvent::ThinkingComplete`
//! - `ControlEvent::ToolCall { ... }` → `AgentEvent::ToolCallStarted`
//! - `ControlEvent::ToolResult { ... }` → `AgentEvent::ToolCallFinished`
//! - `ControlEvent::ToolNeedsApproval { ... }` → auto-approve
//! - `ControlEvent::SessionState { Completed | Cancelled }` → `AgentEvent::TurnComplete`
//! - `ControlEvent::AgentError` / `NodeError` → `AgentEvent::Error`

use std::sync::Arc;

use futures::StreamExt;
use serde::Serialize;
use sven_core::AgentEvent;
use sven_tools::{ToolCall, ToolSchema};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::agent::AgentRequest;
use crate::control::{ControlCommand as Cmd, ControlEvent as Evt};

// ── Public entry points ────────────────────────────────────────────────────────

/// Background task that bridges a frontend to a running sven node via WebSocket.
///
/// Replaces `agent_task` when `SVEN_NODE_URL` and `SVEN_NODE_TOKEN` are
/// present in the environment.
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

    loop {
        let req = match rx.recv().await {
            Some(r) => r,
            None => break,
        };

        let content = match req {
            AgentRequest::Submit { content, .. } => content,
            AgentRequest::Resubmit {
                new_user_content: content,
                ..
            } => content,
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
                        handle_event(evt, &tx, &ws_out_tx, Uuid::nil(), &mut String::new()).await;
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
                mode: "agent".to_string(),
                working_dir: None,
            },
        )
        .is_err()
        {
            let _ = tx.send(AgentEvent::Error("WS send failed".into())).await;
            break;
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

        let mut thinking_buf = String::new();
        loop {
            tokio::select! {
                msg = ws_stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => {
                            let _ = tx.send(AgentEvent::Error(format!("WS recv: {e}"))).await;
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
                    let done = handle_event(evt, &tx, &ws_out_tx, sid, &mut thinking_buf).await;
                    if done {
                        break;
                    }
                }
                Ok(()) = &mut cancel_rx => {
                    let _ = send_cmd(&ws_out_tx, &Cmd::CancelSession { session_id: sid });
                    let _ = tx.send(AgentEvent::Aborted { partial_text: String::new() }).await;
                    break;
                }
            }
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
    thinking_buf: &mut String,
) -> bool {
    match evt {
        Evt::OutputDelta { delta, role, .. } => {
            if role == "thinking" {
                thinking_buf.push_str(&delta);
                let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
            } else {
                if !thinking_buf.is_empty() {
                    let content = std::mem::take(thinking_buf);
                    let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
                }
                let _ = tx.send(AgentEvent::TextDelta(delta)).await;
            }
        }
        Evt::OutputComplete { text, role, .. } => {
            if role == "thinking" {
                thinking_buf.push_str(&text);
            } else {
                if !thinking_buf.is_empty() {
                    let content = std::mem::take(thinking_buf);
                    let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
                }
                let _ = tx.send(AgentEvent::TextComplete(text)).await;
            }
        }
        Evt::ToolCall {
            call_id,
            tool_name,
            args,
            ..
        } => {
            if !thinking_buf.is_empty() {
                let content = std::mem::take(thinking_buf);
                let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
            }
            let tc = ToolCall {
                id: call_id,
                name: tool_name,
                args,
            };
            let _ = tx.send(AgentEvent::ToolCallStarted(tc)).await;
        }
        Evt::ToolResult {
            call_id,
            output,
            is_error,
            ..
        } => {
            let _ = tx
                .send(AgentEvent::ToolCallFinished {
                    call_id,
                    tool_name: String::new(),
                    output,
                    is_error,
                })
                .await;
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
            if state == "completed" || state == "cancelled" {
                if !thinking_buf.is_empty() {
                    let content = std::mem::take(thinking_buf);
                    let _ = tx.send(AgentEvent::ThinkingComplete(content)).await;
                }
                let _ = tx.send(AgentEvent::TurnComplete).await;
                return true;
            }
        }
        Evt::AgentError { message, .. } => {
            let _ = tx.send(AgentEvent::Error(message)).await;
            return true;
        }
        Evt::NodeError { message, .. } => {
            let _ = tx.send(AgentEvent::Error(message)).await;
            return true;
        }
        Evt::ToolList { .. } | Evt::SessionList { .. } | Evt::Unknown => {}
        Evt::PeerList { peers } => {
            let peer_infos = peers
                .into_iter()
                .map(|p| sven_core::PeerInfo {
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
