// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! WebAuthn passkey authentication handlers and JWT session issuance.
//!
//! # Registration flow (new device)
//!
//! ```text
//! POST /web/auth/register/challenge
//!   → {challenge_id, publicKey: {...}}   (challenge stored in ChallengeStore)
//!
//! POST /web/auth/register/complete
//!   body: {challenge_id, credential: <PublicKeyCredential JSON>}
//!   → {device_id}                        (device stored as Pending)
//! ```
//!
//! # Login flow (approved device)
//!
//! ```text
//! POST /web/auth/login/challenge
//!   body: {device_id}
//!   → {challenge_id, publicKey: {...}}
//!
//! POST /web/auth/login/complete
//!   body: {challenge_id, credential: <PublicKeyCredential JSON>}
//!   → 200 OK  +  Set-Cookie: sven_session=<JWT>; HttpOnly; Secure; SameSite=Strict
//! ```
//!
//! # JWT
//!
//! Sessions are represented as short-lived JWTs stored in an HttpOnly cookie.
//! The secret key is generated at node startup and lives only in memory —
//! restarting the node invalidates all active sessions.

pub mod challenge;
pub mod devices;

use std::sync::Arc;

use axum::response::sse::Event;
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response, Sse},
    Json,
};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::CookieJar;
use chrono::Utc;
use futures::stream;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};
use url::Url;
use uuid::Uuid;
use webauthn_rs::prelude::*;

use challenge::{ChallengeState, ChallengeStore};
use devices::{DeviceRegistry, DeviceStatus};

// ── JWT claims ────────────────────────────────────────────────────────────────

/// Claims embedded in the session JWT cookie.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Device UUID (subject).
    pub sub: String,
    /// Expiry (Unix timestamp).
    pub exp: i64,
    /// JWT ID (unique nonce, for future revocation support).
    pub jti: String,
}

// ── Auth state shared across all web auth handlers ────────────────────────────

/// Shared authentication state passed via Axum `State`.
#[derive(Clone)]
pub struct WebAuthState {
    pub webauthn: Arc<Webauthn>,
    pub challenges: ChallengeStore,
    pub devices: DeviceRegistry,
    pub jwt_encoding: EncodingKey,
    pub jwt_decoding: DecodingKey,
    pub session_ttl_secs: u64,
    /// Canonical origin the node is configured for (e.g. `https://host.ts.net:18790`).
    /// Returned by `/web/auth/info` so the frontend can redirect when accessed
    /// from the wrong origin (e.g. `https://localhost:18790`).
    pub rp_origin: Arc<String>,
    /// Approval broadcast — pending devices subscribe here.
    pub approval_tx: broadcast::Sender<Uuid>,
}

impl WebAuthState {
    /// Build a new `WebAuthState` from config values.
    pub fn new(
        rp_id: &str,
        rp_origin: &str,
        rp_name: &str,
        devices: DeviceRegistry,
        session_ttl_secs: u64,
    ) -> anyhow::Result<Self> {
        let origin = Url::parse(rp_origin)
            .map_err(|e| anyhow::anyhow!("invalid rp_origin {rp_origin:?}: {e}"))?;

        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|e| anyhow::anyhow!("WebauthnBuilder: {e}"))?
            .rp_name(rp_name)
            .build()
            .map_err(|e| anyhow::anyhow!("Webauthn::build: {e}"))?;

        // Generate a random HMAC-SHA256 key for session JWTs.
        // Key is ephemeral — sessions are invalidated on node restart.
        let secret = generate_jwt_secret();
        let jwt_encoding = EncodingKey::from_secret(&secret);
        let jwt_decoding = DecodingKey::from_secret(&secret);

        let (challenges, _sweep_handle) = ChallengeStore::new();
        let (approval_tx, _) = broadcast::channel(64);

        Ok(Self {
            webauthn: Arc::new(webauthn),
            challenges,
            devices,
            jwt_encoding,
            jwt_decoding,
            session_ttl_secs,
            rp_origin: Arc::new(rp_origin.to_string()),
            approval_tx,
        })
    }

    /// Issue a signed session JWT for an approved device.
    pub fn issue_jwt(&self, device_id: Uuid) -> anyhow::Result<String> {
        let exp = Utc::now().timestamp() + self.session_ttl_secs as i64;
        let claims = SessionClaims {
            sub: device_id.to_string(),
            exp,
            jti: Uuid::new_v4().to_string(),
        };
        jsonwebtoken::encode(&Header::default(), &claims, &self.jwt_encoding)
            .map_err(|e| anyhow::anyhow!("JWT encode: {e}"))
    }

    /// Verify a session JWT and return the device UUID.
    pub fn verify_jwt(&self, token: &str) -> Option<Uuid> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;
        let data =
            jsonwebtoken::decode::<SessionClaims>(token, &self.jwt_decoding, &validation).ok()?;
        data.claims.sub.parse::<Uuid>().ok()
    }
}

// ── Request / response shapes ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterChallengeRequest {
    /// Human-readable name the user gives this device.
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterChallengeResponse {
    /// Opaque challenge ID — must be echoed back in the completion request.
    pub challenge_id: String,
    /// WebAuthn `PublicKeyCredentialCreationOptions` to pass to
    /// `navigator.credentials.create()`.
    pub public_key: CreationChallengeResponse,
}

#[derive(Debug, Deserialize)]
pub struct RegisterCompleteRequest {
    pub challenge_id: String,
    /// The `RegisterPublicKeyCredential` returned by `navigator.credentials.create()`.
    pub credential: RegisterPublicKeyCredential,
    /// Human-readable display name (fallback to "unnamed device").
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterCompleteResponse {
    /// The assigned device UUID, shown to the admin for approval.
    pub device_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginChallengeRequest {
    /// Device UUID obtained during registration.
    pub device_id: String,
}

#[derive(Debug, Serialize)]
pub struct LoginChallengeResponse {
    pub challenge_id: String,
    pub public_key: RequestChallengeResponse,
}

#[derive(Debug, Deserialize)]
pub struct LoginCompleteRequest {
    pub challenge_id: String,
    pub credential: PublicKeyCredential,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

/// POST /web/auth/register/challenge
///
/// Issues a WebAuthn registration challenge for a new device.
pub async fn register_challenge(
    State(auth): State<WebAuthState>,
    Json(req): Json<RegisterChallengeRequest>,
) -> Response {
    let device_id = Uuid::new_v4();
    let display_name = req
        .display_name
        .unwrap_or_else(|| format!("device-{}", &device_id.to_string()[..8]));

    // webauthn-rs requires a Vec<u8> user ID — use the UUID bytes.
    let user_id = device_id.as_bytes().to_vec();
    let user_name = display_name.clone();

    let result =
        auth.webauthn
            .start_passkey_registration(device_id, &user_name, &display_name, None);

    match result {
        Ok((ccr, reg_state)) => {
            let challenge_id = auth.challenges.insert(ChallengeState::Registration {
                state: reg_state,
                device_id,
            });
            let _ = user_id; // device_id bytes consumed above
            Json(RegisterChallengeResponse {
                challenge_id: challenge_id.to_string(),
                public_key: ccr,
            })
            .into_response()
        }
        Err(e) => {
            warn!("WebAuthn registration challenge failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "challenge generation failed",
            )
                .into_response()
        }
    }
}

/// POST /web/auth/register/complete
///
/// Completes WebAuthn registration: verifies the attestation and stores the
/// new device as `Pending`.
pub async fn register_complete(
    State(auth): State<WebAuthState>,
    Json(req): Json<RegisterCompleteRequest>,
) -> Response {
    let challenge_id: Uuid = match req.challenge_id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid challenge_id").into_response(),
    };

    let (reg_state, device_id) = match auth.challenges.take(challenge_id) {
        Some(ChallengeState::Registration { state, device_id }) => (state, device_id),
        Some(_) => return (StatusCode::BAD_REQUEST, "challenge type mismatch").into_response(),
        None => {
            return (StatusCode::BAD_REQUEST, "unknown or expired challenge_id").into_response()
        }
    };

    let passkey = match auth
        .webauthn
        .finish_passkey_registration(&req.credential, &reg_state)
    {
        Ok(pk) => pk,
        Err(e) => {
            warn!(device_id = %device_id, "WebAuthn registration verification failed: {e}");
            return (StatusCode::UNAUTHORIZED, "registration verification failed").into_response();
        }
    };

    let display_name = req
        .display_name
        .unwrap_or_else(|| format!("device-{}", &device_id.to_string()[..8]));

    match auth
        .devices
        .register(device_id, display_name, passkey)
        .await
    {
        Ok(_) => {
            info!(device_id = %device_id, "new web device registered, awaiting approval");
            Json(RegisterCompleteResponse {
                device_id: device_id.to_string(),
            })
            .into_response()
        }
        Err(e) => {
            warn!("device registry write failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, "registry write failed").into_response()
        }
    }
}

/// POST /web/auth/login/challenge
///
/// Issues a WebAuthn authentication challenge for a known device.
pub async fn login_challenge(
    State(auth): State<WebAuthState>,
    Json(req): Json<LoginChallengeRequest>,
) -> Response {
    let device_id: Uuid = match req.device_id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid device_id").into_response(),
    };

    let device = match auth.devices.get(device_id).await {
        Some(d) => d,
        None => return (StatusCode::NOT_FOUND, "device not found").into_response(),
    };

    if device.status == DeviceStatus::Revoked {
        return (StatusCode::FORBIDDEN, "device revoked").into_response();
    }

    let result = auth
        .webauthn
        .start_passkey_authentication(std::slice::from_ref(&device.credential));

    match result {
        Ok((rcr, auth_state)) => {
            let challenge_id = auth.challenges.insert(ChallengeState::Authentication {
                state: auth_state,
                device_id,
            });
            Json(LoginChallengeResponse {
                challenge_id: challenge_id.to_string(),
                public_key: rcr,
            })
            .into_response()
        }
        Err(e) => {
            warn!(device_id = %device_id, "WebAuthn login challenge failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "challenge generation failed",
            )
                .into_response()
        }
    }
}

/// POST /web/auth/login/complete
///
/// Completes WebAuthn authentication. On success, sets the session JWT cookie.
pub async fn login_complete(
    State(auth): State<WebAuthState>,
    jar: CookieJar,
    Json(req): Json<LoginCompleteRequest>,
) -> Response {
    let challenge_id: Uuid = match req.challenge_id.parse() {
        Ok(id) => id,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid challenge_id").into_response(),
    };

    let (auth_state, device_id) = match auth.challenges.take(challenge_id) {
        Some(ChallengeState::Authentication { state, device_id }) => (state, device_id),
        Some(_) => return (StatusCode::BAD_REQUEST, "challenge type mismatch").into_response(),
        None => {
            return (StatusCode::BAD_REQUEST, "unknown or expired challenge_id").into_response()
        }
    };

    let device = match auth.devices.get(device_id).await {
        Some(d) => d,
        None => return (StatusCode::NOT_FOUND, "device not found").into_response(),
    };

    if device.status != DeviceStatus::Approved {
        return match device.status {
            DeviceStatus::Pending => (StatusCode::FORBIDDEN, "device awaiting approval"),
            DeviceStatus::Revoked => (StatusCode::FORBIDDEN, "device revoked"),
            DeviceStatus::Approved => unreachable!(),
        }
        .into_response();
    }

    let auth_result = match auth
        .webauthn
        .finish_passkey_authentication(&req.credential, &auth_state)
    {
        Ok(r) => r,
        Err(e) => {
            warn!(device_id = %device_id, "WebAuthn login verification failed: {e}");
            return (StatusCode::UNAUTHORIZED, "authentication failed").into_response();
        }
    };

    // Update the stored credential counter (anti-cloning defence).
    if auth_result.needs_update() {
        let mut updated_cred = device.credential.clone();
        updated_cred.update_credential(&auth_result);
        let _ = auth
            .devices
            .update_credential(device_id, updated_cred)
            .await;
    }
    let _ = auth.devices.touch(device_id).await;

    // Issue session JWT as an HttpOnly cookie.
    let token = match auth.issue_jwt(device_id) {
        Ok(t) => t,
        Err(e) => {
            warn!("JWT issuance failed: {e}");
            return (StatusCode::INTERNAL_SERVER_ERROR, "session issuance failed").into_response();
        }
    };

    let cookie = Cookie::build(("sven_session", token))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/web")
        .build();

    info!(device_id = %device_id, "web device authenticated");
    (jar.add(cookie), StatusCode::OK).into_response()
}

/// GET /web/auth/status?device=<uuid>
///
/// Server-Sent Events stream that fires once when a pending device is approved.
/// The browser subscribes to this after registration to know when to proceed.
pub async fn device_status_sse(
    State(auth): State<WebAuthState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Response {
    let device_id: Uuid = match params.get("device").and_then(|s| s.parse().ok()) {
        Some(id) => id,
        None => {
            return (StatusCode::BAD_REQUEST, "missing or invalid device param").into_response()
        }
    };

    // Check current status first — device might already be approved.
    if let Some(device) = auth.devices.get(device_id).await {
        match device.status {
            DeviceStatus::Approved => {
                let stream = stream::once(async {
                    Ok::<Event, std::convert::Infallible>(
                        Event::default().event("approved").data("approved"),
                    )
                });
                return Sse::new(stream).into_response();
            }
            DeviceStatus::Revoked => {
                let stream = stream::once(async {
                    Ok::<Event, std::convert::Infallible>(
                        Event::default().event("revoked").data("revoked"),
                    )
                });
                return Sse::new(stream).into_response();
            }
            DeviceStatus::Pending => {}
        }
    }

    // Subscribe to the approval broadcast and wait.
    let mut rx = auth.approval_tx.subscribe();
    let stream = async_stream::stream! {
        // Send a heartbeat every 30s to keep the connection alive.
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                Ok(approved_id) = rx.recv() => {
                    if approved_id == device_id {
                        yield Ok::<Event, std::convert::Infallible>(
                            Event::default().event("approved").data("approved")
                        );
                        break;
                    }
                }
                _ = ticker.tick() => {
                    yield Ok(Event::default().comment("heartbeat"));
                }
            }
        }
    };
    Sse::new(stream).into_response()
}

// ── Auth info ─────────────────────────────────────────────────────────────────

/// Response for GET /web/auth/info
#[derive(Debug, Serialize)]
pub struct AuthInfoResponse {
    /// The canonical origin this node is configured for.
    /// The browser should redirect here if its current origin does not match.
    pub rp_origin: String,
}

/// GET /web/auth/info
///
/// Returns the canonical `rp_origin` so the frontend can detect when it is
/// being served from the wrong origin (e.g. `https://localhost:18790` instead
/// of `https://myhost.tail1234.ts.net:18790`) and redirect automatically.
pub async fn auth_info(State(auth): State<WebAuthState>) -> Response {
    Json(AuthInfoResponse {
        rp_origin: (*auth.rp_origin).clone(),
    })
    .into_response()
}

// ── Session cookie extractor ──────────────────────────────────────────────────

/// Extract and verify the session cookie, returning the device UUID.
///
/// Returns `None` if the cookie is missing or the JWT is invalid/expired.
pub fn extract_session(auth: &WebAuthState, jar: &CookieJar) -> Option<Uuid> {
    let token = jar.get("sven_session")?.value();
    auth.verify_jwt(token)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn generate_jwt_secret() -> Vec<u8> {
    use rand::RngCore;
    let mut bytes = vec![0u8; 64];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    bytes
}
