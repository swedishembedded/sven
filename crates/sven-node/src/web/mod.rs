// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Web terminal — browser-accessible PTY sessions via WebAuthn passkey auth.
//!
//! # Route map
//!
//! | Method | Path                             | Handler                      | Auth       |
//! |--------|----------------------------------|------------------------------|------------|
//! | GET    | `/web`                           | Redirect → `/web/`           | None       |
//! | GET    | `/web/`                          | `index.html`                 | None       |
//! | GET    | `/web/assets/*`                  | Embedded static files        | None       |
//! | POST   | `/web/auth/register/challenge`   | WebAuthn registration start  | None       |
//! | POST   | `/web/auth/register/complete`    | WebAuthn registration finish | None       |
//! | POST   | `/web/auth/login/challenge`      | WebAuthn login start         | None       |
//! | POST   | `/web/auth/login/complete`       | WebAuthn login finish        | None       |
//! | GET    | `/web/auth/status`               | SSE approval stream          | None       |
//! | GET    | `/web/pty/ws`                    | PTY WebSocket bridge         | Session JWT|
//!
//! # CSRF
//!
//! The `/web/auth/*` routes are exempt from the node's CSRF guard because
//! WebAuthn has its own CSRF protection built in (single-use challenge nonces
//! bound to the session).  The CSRF guard is still applied to all other routes.

pub mod assets;
pub mod auth;
pub mod pty;
pub mod ws;

use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use axum_extra::extract::CookieJar;
use tracing::warn;

use assets::WebAssets;
use auth::devices::DeviceStatus;
use auth::{
    auth_info, device_status_sse, extract_session, login_challenge, login_complete,
    register_challenge, register_complete, WebAuthState,
};
use pty::manager::PtyManager;

// ── Combined web state ────────────────────────────────────────────────────────

/// All state needed by the web terminal routes.
///
/// Clone is cheap — all fields are either `Arc` or trivially copyable.
#[derive(Clone)]
pub struct WebState {
    pub auth: WebAuthState,
    pub pty_manager: PtyManager,
}

// ── Router ────────────────────────────────────────────────────────────────────

/// Build the `/web` Axum router.
///
/// Call `app.merge(web_router(state))` in `http::serve()`.
pub fn web_router(state: WebState) -> Router {
    // Static asset handler — no auth, no CSRF.
    let assets = Router::new()
        .route("/web/", get(serve_index))
        .route("/web", get(|| async { Redirect::permanent("/web/") }))
        .route("/web/assets/*path", get(serve_asset))
        .with_state(state.clone());

    // Auth routes — no CSRF (WebAuthn provides its own protection).
    let auth_routes = Router::new()
        .route("/web/auth/info", get(auth_info))
        .route("/web/auth/register/challenge", post(register_challenge))
        .route("/web/auth/register/complete", post(register_complete))
        .route("/web/auth/login/challenge", post(login_challenge))
        .route("/web/auth/login/complete", post(login_complete))
        .route("/web/auth/status", get(device_status_sse))
        .with_state(state.auth.clone());

    // PTY WebSocket — requires valid session cookie.
    let pty_routes = Router::new()
        .route("/web/pty/ws", get(pty_ws_handler))
        .with_state(state.clone());

    assets.merge(auth_routes).merge(pty_routes)
}

// ── Static asset handlers ─────────────────────────────────────────────────────

async fn serve_index() -> Response {
    serve_embedded("index.html").await
}

async fn serve_asset(Path(path): Path<String>) -> Response {
    serve_embedded(&path).await
}

async fn serve_embedded(path: &str) -> Response {
    match WebAssets::get(path) {
        Some(content) => {
            let mime = mime_type_for(path);
            (
                [(header::CONTENT_TYPE, HeaderValue::from_static(mime))],
                content.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn mime_type_for(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

// ── PTY WebSocket handler ─────────────────────────────────────────────────────

async fn pty_ws_handler(
    upgrade: WebSocketUpgrade,
    State(state): State<WebState>,
    jar: CookieJar,
) -> Response {
    // Verify session cookie.
    let device_id = match extract_session(&state.auth, &jar) {
        Some(id) => id,
        None => {
            return (StatusCode::UNAUTHORIZED, "session required").into_response();
        }
    };

    // Check device is still approved.
    match state.auth.devices.get(device_id).await {
        Some(d) if d.status == DeviceStatus::Approved => {}
        Some(d) => {
            warn!(device_id = %device_id, status = %d.status, "pty ws: device not approved");
            return (StatusCode::FORBIDDEN, "device not approved").into_response();
        }
        None => return (StatusCode::NOT_FOUND, "device not found").into_response(),
    }

    let cols = state.pty_manager.default_cols();
    let rows = state.pty_manager.default_rows();

    // Spawn a new PTY session (or reattach via tmux -A).
    let session = match state.pty_manager.spawn(device_id, cols, rows).await {
        Ok(s) => s,
        Err(e) => {
            warn!(device_id = %device_id, "PTY spawn failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "PTY spawn failed").into_response();
        }
    };

    let manager = state.pty_manager.clone();
    upgrade.on_upgrade(move |socket| ws::handle_pty_socket(socket, session, manager, device_id))
}
