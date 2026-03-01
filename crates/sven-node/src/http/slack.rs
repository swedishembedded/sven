// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Slack integration: Socket Mode (default) and HTTP Events API (optional).
//!
//! # Socket Mode (recommended, default)
//!
//! Socket Mode uses an outbound WebSocket from sven to Slack's servers. No
//! inbound port is required — only the `appToken` (`xapp-…`) and `botToken`
//! (`xoxb-…`) need to be configured.
//!
//! ```yaml
//! slack:
//!   accounts:
//!     - mode: socket
//!       app_token: "xapp-..."
//!       bot_token: "xoxb-..."
//! ```
//!
//! # HTTP Events API (optional)
//!
//! When `mode: http`, Slack POSTs events to the configured `webhook_path`.
//! Every incoming request is verified via **HMAC-SHA256** using the Slack
//! signing secret:
//!
//! 1. Slack sends `X-Slack-Signature: v0=<hmac>` and
//!    `X-Slack-Request-Timestamp: <unix_ts>`.
//! 2. We compute `HMAC-SHA256(signing_secret, "v0:" + timestamp + ":" + body)`.
//! 3. We compare in constant time (`subtle::ConstantTimeEq`).
//! 4. We reject requests with a timestamp more than 5 minutes old (replay
//!    protection).
//!
//! # Message routing
//!
//! Incoming Slack messages are forwarded to the agent as `SendInput` commands.
//! Agent output is sent back to the same channel via the Slack Web API.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::{
    config::SlackAccount,
    control::{protocol::ControlCommand, service::AgentHandle},
};

// ── HTTP webhook handler ──────────────────────────────────────────────────────

/// State injected into the Slack HTTP handler.
#[derive(Clone)]
pub struct SlackWebhookState {
    /// Signing secret bytes. Never stored as a string after parsing.
    pub signing_secret: Arc<Vec<u8>>,
    pub agent: AgentHandle,
}

/// Axum handler for `POST /slack/events` (and any custom webhook path).
///
/// Verifies the Slack HMAC-SHA256 signature before processing the payload.
pub async fn slack_events_handler(
    State(state): State<SlackWebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // Verify HMAC before touching the body.
    let timestamp = headers
        .get("x-slack-request-timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let provided_sig = headers
        .get("x-slack-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if let Err(e) = verify_slack_signature(&state.signing_secret, timestamp, &body, provided_sig) {
        warn!("Slack signature verification failed: {e}");
        return (StatusCode::UNAUTHORIZED, "invalid signature").into_response();
    }

    // Parse the Slack event payload.
    let payload: SlackPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            warn!("failed to parse Slack payload: {e}");
            return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
        }
    };

    match payload {
        SlackPayload::UrlVerification { challenge } => {
            // Slack sends this once when the webhook is first configured.
            (StatusCode::OK, challenge).into_response()
        }
        SlackPayload::EventCallback { event } => {
            handle_slack_event(event, &state.agent).await;
            StatusCode::OK.into_response()
        }
    }
}

async fn handle_slack_event(event: SlackEvent, agent: &AgentHandle) {
    match event {
        SlackEvent::Message { text, channel, ts } => {
            debug!(channel, ts, "Slack message received");
            // Forward to agent as a new session input.
            // A real implementation would map channel → session_id for
            // conversation continuity; for now we create a new session
            // per message for simplicity.
            let session_id = Uuid::new_v4();
            let _ = agent
                .send(ControlCommand::NewSession {
                    id: session_id,
                    mode: sven_config::AgentMode::Agent,
                    working_dir: None,
                })
                .await;
            let _ = agent
                .send(ControlCommand::SendInput {
                    session_id,
                    text: text.unwrap_or_default(),
                })
                .await;
            // TODO: subscribe and stream response back to Slack channel.
        }
        SlackEvent::Other => {
            debug!("unhandled Slack event type");
        }
    }
}

// ── HMAC-SHA256 signature verification ───────────────────────────────────────

/// Verify a Slack request signature.
///
/// # Security properties
///
/// - Computes `HMAC-SHA256(signing_secret, "v0:" + timestamp + ":" + body)`.
/// - Compares with `subtle::ConstantTimeEq` — no timing oracle.
/// - Rejects requests with a timestamp more than 5 minutes old (replay guard).
pub fn verify_slack_signature(
    signing_secret: &[u8],
    timestamp: &str,
    body: &[u8],
    provided_sig: &str,
) -> Result<(), SlackVerifyError> {
    // Replay protection: reject stale timestamps (±5 minutes).
    let ts: i64 = timestamp
        .parse()
        .map_err(|_| SlackVerifyError::InvalidTimestamp)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if (now - ts).unsigned_abs() > 300 {
        return Err(SlackVerifyError::StaleTimestamp);
    }

    // Compute expected HMAC.
    let mut mac =
        Hmac::<Sha256>::new_from_slice(signing_secret).map_err(|_| SlackVerifyError::Internal)?;
    mac.update(b"v0:");
    mac.update(timestamp.as_bytes());
    mac.update(b":");
    mac.update(body);
    let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

    // Constant-time comparison — same length guaranteed by hex encoding.
    if expected
        .as_bytes()
        .ct_eq(provided_sig.as_bytes())
        .unwrap_u8()
        != 1
    {
        return Err(SlackVerifyError::InvalidSignature);
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum SlackVerifyError {
    #[error("invalid timestamp")]
    InvalidTimestamp,
    #[error("request timestamp is too old (replay protection)")]
    StaleTimestamp,
    #[error("HMAC signature does not match")]
    InvalidSignature,
    #[error("internal HMAC error")]
    Internal,
}

// ── Slack Socket Mode client ──────────────────────────────────────────────────

/// Start a Slack Socket Mode connection for the given account.
///
/// Runs in a background task. Reconnects automatically on disconnect.
/// The channel sends incoming Slack messages to the agent as ControlCommands.
pub async fn run_socket_mode(account: SlackAccount, agent: AgentHandle) {
    let Some(app_token) = account.app_token else {
        error!("Slack Socket Mode requires app_token");
        return;
    };
    let Some(bot_token) = account.bot_token else {
        error!("Slack Socket Mode requires bot_token");
        return;
    };

    info!("Slack Socket Mode: connecting");

    // The full Socket Mode implementation uses Slack's WebSocket URL obtained
    // by calling `apps.connections.open` with the app token. The loop below
    // reconnects on failure.
    loop {
        match connect_socket_mode(&app_token, &bot_token, &agent).await {
            Ok(()) => {
                info!("Slack Socket Mode: connection closed, reconnecting in 5s");
            }
            Err(e) => {
                error!("Slack Socket Mode error: {e}, reconnecting in 10s");
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                continue;
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn connect_socket_mode(
    app_token: &str,
    _bot_token: &str,
    agent: &AgentHandle,
) -> anyhow::Result<()> {
    // Step 1: Call Slack `apps.connections.open` to get the WebSocket URL.
    let wss_url = fetch_socket_mode_url(app_token).await?;
    debug!(url = %wss_url, "Slack Socket Mode: got WebSocket URL");

    // Step 2: Connect to the WebSocket and process events.
    // Using tokio-tungstenite (already available via reqwest's dep tree) or
    // a direct Slack SDK. Here we use a minimal implementation.
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;

    let (ws_stream, _) = connect_async(&wss_url)
        .await
        .map_err(|e| anyhow::anyhow!("WebSocket connect: {e}"))?;
    let (mut sink, mut stream) = ws_stream.split();

    while let Some(msg) = stream.next().await {
        match msg? {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                if let Ok(payload) = serde_json::from_str::<SocketModeEnvelope>(&text) {
                    // Acknowledge immediately.
                    let ack = serde_json::json!({ "envelope_id": payload.envelope_id });
                    let _ = sink
                        .send(tokio_tungstenite::tungstenite::Message::Text(
                            ack.to_string(),
                        ))
                        .await;

                    if let Some(event) = payload.payload {
                        dispatch_socket_mode_event(event, agent).await;
                    }
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }

    Ok(())
}

async fn fetch_socket_mode_url(app_token: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let resp = client
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(app_token)
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    resp.get("url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("apps.connections.open returned no url: {resp}"))
}

async fn dispatch_socket_mode_event(payload: serde_json::Value, agent: &AgentHandle) {
    if let Some(event) = payload.get("event") {
        if event.get("type").and_then(|t| t.as_str()) == Some("message") {
            let text = event
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            let channel = event
                .get("channel")
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            debug!(channel, "Slack Socket Mode: incoming message");

            let session_id = Uuid::new_v4();
            let _ = agent
                .send(ControlCommand::NewSession {
                    id: session_id,
                    mode: sven_config::AgentMode::Agent,
                    working_dir: None,
                })
                .await;
            let _ = agent
                .send(ControlCommand::SendInput { session_id, text })
                .await;
        }
    }
}

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SlackPayload {
    #[serde(rename = "url_verification")]
    UrlVerification { challenge: String },
    #[serde(rename = "event_callback")]
    EventCallback { event: SlackEvent },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SlackEvent {
    #[serde(rename = "message")]
    Message {
        text: Option<String>,
        channel: Option<String>,
        ts: Option<String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct SocketModeEnvelope {
    envelope_id: String,
    payload: Option<serde_json::Value>,
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-signing-secret";

    fn make_valid_sig(secret: &[u8], timestamp: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(b"v0:");
        mac.update(timestamp.as_bytes());
        mac.update(b":");
        mac.update(body);
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn recent_ts() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }

    #[test]
    fn valid_signature_is_accepted() {
        let ts = recent_ts();
        let body = b"test body";
        let sig = make_valid_sig(SECRET, &ts, body);
        assert!(verify_slack_signature(SECRET, &ts, body, &sig).is_ok());
    }

    #[test]
    fn wrong_signature_is_rejected() {
        let ts = recent_ts();
        let body = b"test body";
        let sig = make_valid_sig(SECRET, &ts, body);
        let wrong_sig = sig.replace('a', "b");
        assert!(verify_slack_signature(SECRET, &ts, body, &wrong_sig).is_err());
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let old_ts = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 400)
            .to_string();
        let body = b"body";
        let sig = make_valid_sig(SECRET, &old_ts, body);
        let result = verify_slack_signature(SECRET, &old_ts, body, &sig);
        assert!(matches!(result, Err(SlackVerifyError::StaleTimestamp)));
    }

    #[test]
    fn tampered_body_is_rejected() {
        let ts = recent_ts();
        let original_body = b"original";
        let sig = make_valid_sig(SECRET, &ts, original_body);
        let tampered_body = b"tampered";
        assert!(verify_slack_signature(SECRET, &ts, tampered_body, &sig).is_err());
    }

    #[test]
    fn wrong_signing_secret_is_rejected() {
        let ts = recent_ts();
        let body = b"body";
        let sig = make_valid_sig(b"correct-secret", &ts, body);
        assert!(verify_slack_signature(b"wrong-secret", &ts, body, &sig).is_err());
    }

    #[test]
    fn missing_v0_prefix_is_rejected() {
        let ts = recent_ts();
        let body = b"body";
        // Provide the raw hex without the v0= prefix.
        let raw_hex = {
            let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
            mac.update(b"v0:");
            mac.update(ts.as_bytes());
            mac.update(b":");
            mac.update(body);
            hex::encode(mac.finalize().into_bytes())
        };
        // Without "v0=" prefix this won't match.
        assert!(verify_slack_signature(SECRET, &ts, body, &raw_hex).is_err());
    }
}
