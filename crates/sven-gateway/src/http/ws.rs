// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! WebSocket bridge — translates browser WebSocket connections to the internal
//! `ControlCommand` / `ControlEvent` protocol.
//!
//! # Why this exists
//!
//! Web browsers cannot speak libp2p. This handler is a thin shim so the
//! web control UI can control the agent without any installed software.
//!
//! # Wire format
//!
//! JSON over WebSocket (text frames). This is comfortable for browsers and
//! avoids the need for a CBOR library in the browser bundle.
//!
//! # Security
//!
//! Authentication happens before the WebSocket upgrade via the bearer token
//! middleware (see `auth.rs`). By the time `ws_handler` runs, the request
//! is already authenticated. The WebSocket itself adds no additional auth
//! — it inherits the HTTP connection's auth.
//!
//! # Role enforcement
//!
//! The HTTP layer currently grants full operator access to anyone with a
//! valid token. Future work could issue scoped tokens (observer-only).

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::Response,
};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::control::{
    protocol::{ControlCommand, ControlEvent},
    service::AgentHandle,
};

/// HTTP handler for GET /ws.
///
/// Upgrades to WebSocket, then bridges JSON ↔ ControlCommand/ControlEvent.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(agent): State<AgentHandle>,
) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, agent))
}

/// Publicly accessible socket handler for direct use from HTTP router.
pub async fn handle_socket(mut socket: WebSocket, agent: AgentHandle) {
    let mut events = agent.subscribe();

    loop {
        tokio::select! {
            // Incoming message from the browser.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ControlCommand>(&text) {
                            Ok(cmd) => {
                                if let Err(e) = agent.send(cmd).await {
                                    warn!("failed to forward command: {e}");
                                    break;
                                }
                            }
                            Err(e) => {
                                let err = ControlEvent::GatewayError {
                                    code: 400,
                                    message: format!("invalid JSON command: {e}"),
                                };
                                send_event(&mut socket, &err).await;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(data))) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {} // binary frames ignored
                    Some(Err(e)) => {
                        debug!("WebSocket recv error: {e}");
                        break;
                    }
                }
            }
            // Outgoing event from the agent.
            result = events.recv() => {
                match result {
                    Ok(ev) => {
                        send_event(&mut socket, &ev).await;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket operator lagged by {n} events");
                        let err = ControlEvent::GatewayError {
                            code: 503,
                            message: format!("event stream lagged by {n} events"),
                        };
                        send_event(&mut socket, &err).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }

    debug!("WebSocket connection closed");
}

async fn send_event(socket: &mut WebSocket, ev: &ControlEvent) {
    if let Ok(json) = serde_json::to_string(ev) {
        let _ = socket.send(Message::Text(json)).await;
    }
}
