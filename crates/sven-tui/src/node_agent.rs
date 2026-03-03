// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Node-proxy agent backend for the sven TUI.
//!
//! When `SVEN_NODE_URL and SVEN_NODE_TOKEN are set (injected by a
//! running node into the PTY session environment), this module replaces the
//! local agent with a thin WebSocket bridge to the running node
//! agent.  That agent has a live `P2pHandle`, so all peer tools
//! (`list_peers`, `delegate_task`, `send_message`, …) are available.
//!
//! # Protocol
//!
//! The bridge speaks the node's JSON-over-WebSocket control protocol:
//!
//! - `AgentRequest::Submit { content }` → `NewSession` + `SendInput`
//! - `ControlEvent::OutputDelta { role: "assistant" }` → `AgentEvent::TextDelta`
//! - `ControlEvent::OutputDelta { role: "thinking" }` → `AgentEvent::ThinkingDelta`
//! - `ControlEvent::OutputComplete { role: "assistant" }` → `AgentEvent::TextComplete`
//! - `ControlEvent::OutputComplete { role: "thinking" }` → `AgentEvent::ThinkingComplete`
//! - `ControlEvent::ToolCall { … }` → `AgentEvent::ToolCallStarted`
//! - `ControlEvent::ToolResult { … }` → `AgentEvent::ToolCallFinished`
//! - `ControlEvent::ToolNeedsApproval { … }` → auto-approve
//! - `ControlEvent::SessionState { Completed | Cancelled }` → `AgentEvent::TurnComplete`
//! - `ControlEvent::AgentError` / `NodeError` → `AgentEvent::Error`

use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sven_core::AgentEvent;
use sven_tools::ToolCall;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::agent::AgentRequest;

// ── Minimal control protocol types ────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Cmd {
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
    ApproveTool {
        session_id: Uuid,
        call_id: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Evt {
    OutputDelta {
        #[allow(dead_code)]
        session_id: Uuid,
        delta: String,
        role: String,
    },
    OutputComplete {
        #[allow(dead_code)]
        session_id: Uuid,
        text: String,
        role: String,
    },
    ToolCall {
        #[allow(dead_code)]
        session_id: Uuid,
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    ToolResult {
        #[allow(dead_code)]
        session_id: Uuid,
        call_id: String,
        output: String,
        is_error: bool,
    },
    ToolNeedsApproval {
        #[allow(dead_code)]
        session_id: Uuid,
        call_id: String,
        tool_name: String,
        #[allow(dead_code)]
        args: serde_json::Value,
    },
    SessionState {
        #[allow(dead_code)]
        session_id: Uuid,
        state: String,
    },
    AgentError {
        #[allow(dead_code)]
        session_id: Option<Uuid>,
        message: String,
    },
    NodeError {
        #[allow(dead_code)]
        code: u32,
        message: String,
    },
    #[serde(other)]
    Unknown,
}

// ── Public entry point ─────────────────────────────────────────────────────────

/// Background task that bridges the TUI to a running sven node via WebSocket.
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
    use tokio_tungstenite::connect_async_tls_with_config;
    use tungstenite::http::Request;

    let connector = build_tls_connector(insecure);

    let request = match Request::builder()
        .uri(&node_url)
        .header("Authorization", format!("Bearer {node_token}"))
        .header("Host", "127.0.0.1")
        .header("Upgrade", "websocket")
        .header("Connection", "Upgrade")
        .header("Sec-WebSocket-Key", generate_ws_key())
        .header("Sec-WebSocket-Version", "13")
        .body(())
    {
        Ok(r) => r,
        Err(e) => {
            let _ = tx
                .send(AgentEvent::Error(format!("WS request build: {e}")))
                .await;
            return;
        }
    };

    let (ws_stream, _) = match connect_async_tls_with_config(request, None, false, connector).await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = tx
                .send(AgentEvent::Error(format!(
                    "Could not connect to node at {node_url}: {e}"
                )))
                .await;
            return;
        }
    };

    let (ws_sink, mut ws_stream) = ws_stream.split();

    // Use a channel + writer task so we can send from multiple places without
    // holding a lock across await points.
    let (ws_out_tx, mut ws_out_rx) = mpsc::unbounded_channel::<String>();

    // Spawn the writer task.
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
            None => break, // TUI dropped the channel
        };

        match req {
            AgentRequest::Submit { content, .. } => {
                let sid = Uuid::new_v4();

                let (cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel::<()>();
                *cancel_handle.lock().await = Some(cancel_tx);

                // Create session on the node.
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

                // Drain events until session completes or is cancelled.
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
                            let done = handle_event(evt, &tx, &ws_out_tx, sid).await;
                            if done { break; }
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
            AgentRequest::LoadHistory(_) | AgentRequest::Resubmit { .. } => {
                debug!("node_agent_task: ignoring LoadHistory/Resubmit (node manages state)");
            }
        }
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────

async fn handle_event(
    evt: Evt,
    tx: &mpsc::Sender<AgentEvent>,
    ws_out_tx: &mpsc::UnboundedSender<String>,
    session_id: Uuid,
) -> bool {
    match evt {
        Evt::OutputDelta { delta, role, .. } => {
            if role == "thinking" {
                let _ = tx.send(AgentEvent::ThinkingDelta(delta)).await;
            } else {
                let _ = tx.send(AgentEvent::TextDelta(delta)).await;
            }
        }
        Evt::OutputComplete { text, role, .. } => {
            if role == "thinking" {
                let _ = tx.send(AgentEvent::ThinkingComplete(text)).await;
            } else {
                let _ = tx.send(AgentEvent::TextComplete(text)).await;
            }
        }
        Evt::ToolCall {
            call_id,
            tool_name,
            args,
            ..
        } => {
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
        Evt::Unknown => {}
    }
    false
}

fn send_cmd(tx: &mpsc::UnboundedSender<String>, cmd: &impl Serialize) -> anyhow::Result<()> {
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

/// Return the TLS connector to use for the WebSocket connection.
///
/// * `insecure = false` (default) → `None`, which lets `tokio-tungstenite`
///   use its built-in rustls client with the system's native root CA store
///   (enabled via the `rustls-tls-native-roots` feature).  Certificate
///   verification is **on** by default.
///
/// * `insecure = true` → a custom verifier that accepts any certificate.
///   This must be explicitly requested via `--insecure` / `SVEN_GATEWAY_INSECURE` (legacy: `SVEN_NODE_INSECURE`).
fn build_tls_connector(insecure: bool) -> Option<tokio_tungstenite::Connector> {
    if !insecure {
        // Use tokio-tungstenite's default rustls connector (native roots).
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
