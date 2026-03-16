// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Generic webhook handlers.
//!
//! # Routes
//!
//! | Method | Path | Description |
//! |--------|------|-------------|
//! | POST | `/hooks/wake` | Wake the main session with an optional message |
//! | POST | `/hooks/agent` | Spawn an isolated agent run with a custom prompt |
//! | POST | `/hooks/{name}` | Custom-mapped named hook |
//!
//! All endpoints require `Authorization: Bearer <token>` where `<token>`
//! matches `hooks.token` in the node configuration.
//!
//! # Gmail Pub/Sub example
//!
//! Configure Gmail push notifications to POST to `/hooks/gmail`:
//! ```yaml
//! hooks:
//!   token: "${HOOKS_TOKEN}"
//!   mappings:
//!     gmail:
//!       path: "/hooks/gmail"
//!       prompt: "New Gmail notification received. Check your email and summarize."
//! ```
//! Then run: `gcloud pubsub subscriptions create sven-gmail --topic=gmail-push --push-endpoint=https://mynode/hooks/gmail --push-auth-service-account=...`

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::config::HooksConfig;
use crate::control::{protocol::ControlCommand, service::AgentHandle};

/// Shared state for hook handlers.
#[derive(Clone)]
pub struct HooksState {
    pub agent: AgentHandle,
    pub config: HooksConfig,
}

/// Request body for `POST /hooks/wake`.
#[derive(Debug, Deserialize)]
pub struct WakeRequest {
    /// Optional message to inject into the main session.
    pub message: Option<String>,
}

/// Request body for `POST /hooks/agent`.
#[derive(Debug, Deserialize)]
pub struct AgentRequest {
    /// Prompt to run in an isolated agent session.
    pub prompt: String,
}

/// Build the webhook router.
///
/// Returns a [`Router`] with all hook routes wired up.
/// Returns an empty router if no hooks token is configured.
pub fn hooks_router(state: HooksState) -> Router {
    if state.config.token.is_none() {
        debug!("hooks.token not configured — webhook endpoints disabled");
        return Router::new();
    }

    Router::new()
        .route("/hooks/wake", post(wake_handler))
        .route("/hooks/agent", post(agent_handler))
        .route("/hooks/:name", post(named_hook_handler))
        .with_state(state)
}

/// Verify the Bearer token from the Authorization header.
fn verify_token(headers: &HeaderMap, expected: &str) -> bool {
    let auth = match headers.get("authorization").and_then(|v| v.to_str().ok()) {
        Some(v) => v,
        None => return false,
    };

    let token = auth.strip_prefix("Bearer ").unwrap_or("");
    // Constant-time comparison using subtle to prevent timing attacks
    use subtle::ConstantTimeEq;
    token.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// `POST /hooks/wake` — wake the main agent session.
async fn wake_handler(
    State(state): State<HooksState>,
    headers: HeaderMap,
    Json(req): Json<WakeRequest>,
) -> impl IntoResponse {
    let expected = match &state.config.token {
        Some(t) => t.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE,
    };

    if !verify_token(&headers, &expected) {
        warn!("hooks/wake: unauthorized request");
        return StatusCode::UNAUTHORIZED;
    }

    let message = req
        .message
        .unwrap_or_else(|| "Webhook triggered: wake event received.".to_string());

    info!(message_len = message.len(), "hooks/wake: waking agent");

    let _ = state
        .agent
        .send(ControlCommand::SendInput {
            session_id: uuid::Uuid::nil(),
            text: message,
        })
        .await;

    StatusCode::ACCEPTED
}

/// `POST /hooks/agent` — run an isolated agent session with a custom prompt.
async fn agent_handler(
    State(state): State<HooksState>,
    headers: HeaderMap,
    Json(req): Json<AgentRequest>,
) -> impl IntoResponse {
    let expected = match &state.config.token {
        Some(t) => t.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE,
    };

    if !verify_token(&headers, &expected) {
        warn!("hooks/agent: unauthorized request");
        return StatusCode::UNAUTHORIZED;
    }

    info!(
        prompt_len = req.prompt.len(),
        "hooks/agent: starting isolated run"
    );

    // Send as a regular SendInput for now; isolated session support
    // is added when sven-node gains full session isolation per hook.
    let _ = state
        .agent
        .send(ControlCommand::SendInput {
            session_id: uuid::Uuid::new_v4(),
            text: req.prompt,
        })
        .await;

    StatusCode::ACCEPTED
}

/// `POST /hooks/{name}` — named webhook with configured prompt template.
async fn named_hook_handler(
    State(state): State<HooksState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let expected = match &state.config.token {
        Some(t) => t.clone(),
        None => return StatusCode::SERVICE_UNAVAILABLE,
    };

    if !verify_token(&headers, &expected) {
        warn!(hook = %name, "hooks/{name}: unauthorized request");
        return StatusCode::UNAUTHORIZED;
    }

    let mapping = match state.config.mappings.get(&name) {
        Some(m) => m.clone(),
        None => {
            warn!(hook = %name, "hooks/{name}: no mapping configured");
            return StatusCode::NOT_FOUND;
        }
    };

    // Substitute {payload} in the prompt template
    let prompt = mapping.prompt.replace("{payload}", &body);
    info!(hook = %name, prompt_len = prompt.len(), "hooks/{name}: triggering agent");

    let session_id = if mapping.isolated {
        uuid::Uuid::new_v4()
    } else {
        uuid::Uuid::nil()
    };

    let _ = state
        .agent
        .send(ControlCommand::SendInput {
            session_id,
            text: prompt,
        })
        .await;

    StatusCode::ACCEPTED
}
