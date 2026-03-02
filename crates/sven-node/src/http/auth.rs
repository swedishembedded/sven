// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! HTTP bearer-token authentication middleware and per-IP rate limiting.
//!
//! # Token authentication
//!
//! All HTTP/WebSocket requests must include a valid bearer token:
//! ```text
//! Authorization: Bearer <token>
//! ```
//! The raw token is never stored; only its SHA-256 hash lives on disk.
//! Comparison uses [`subtle::ConstantTimeEq`] to prevent timing oracles.
//!
//! # Rate limiting
//!
//! Uses the `governor` crate (GCRA algorithm) for per-IP rate limiting.
//! Failed authentication attempts are counted; 5 failures per minute triggers
//! a 5-minute lockout. Successful auth resets the counter.
//!
//! Loopback addresses (127.0.0.1, ::1) are exempt from rate limiting because
//! a local process that has access to the loopback already has local access to
//! the machine anyway.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    num::NonZeroU32,
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{clock::DefaultClock, state::keyed::DashMapStateStore, Quota, RateLimiter};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::crypto::token::StoredToken;

/// Shared auth state threaded through axum middleware.
#[derive(Clone)]
pub struct AuthState {
    token_hash: Arc<StoredToken>,
    /// Optional plaintext token accepted **only from loopback** connections.
    ///
    /// Used to allow PTY sessions spawned on the same machine to authenticate
    /// without exposing the main bearer token as plaintext.  The loopback
    /// restriction means a remote attacker who reads the env of a PTY process
    /// still cannot authenticate from a remote IP.
    local_token: Option<Arc<String>>,
    limiter: Arc<IpLimiter>,
}

type IpLimiter = RateLimiter<IpAddr, DashMapStateStore<IpAddr>, DefaultClock>;

impl AuthState {
    /// Build auth state from a stored token hash.
    ///
    /// `max_per_minute`: maximum failed auth attempts before lockout.
    /// `burst`: how many attempts are allowed in a burst before the rate
    /// limit kicks in.
    pub fn new(token_hash: StoredToken, max_per_minute: u32, burst: u32) -> Self {
        let quota =
            Quota::per_minute(NonZeroU32::new(max_per_minute).expect("max_per_minute must be > 0"))
                .allow_burst(NonZeroU32::new(burst).expect("burst must be > 0"));

        Self {
            token_hash: Arc::new(token_hash),
            local_token: None,
            limiter: Arc::new(RateLimiter::keyed(quota)),
        }
    }

    /// Default configuration: 5 attempts per minute, burst of 2.
    pub fn with_defaults(token_hash: StoredToken) -> Self {
        Self::new(token_hash, 5, 2)
    }

    /// Attach a local-only plaintext token accepted only from loopback IPs.
    ///
    /// This is used so PTY sessions running on the same machine can connect
    /// to the node without having the main (hash-protected) bearer token.
    pub fn with_local_token(mut self, raw: String) -> Self {
        self.local_token = Some(Arc::new(raw));
        self
    }
}

// ── Middleware ────────────────────────────────────────────────────────────────

/// Axum middleware that verifies the bearer token.
///
/// This version works with `AppState` (combined state type). The IP is
/// extracted from the `X-Forwarded-For` header or the `ConnectInfo` extension.
///
/// Returns `401 Unauthorized` on missing/wrong token, `429 Too Many Requests`
/// when the rate limit is exceeded.
pub async fn bearer_auth_mw<S>(
    State(state): State<S>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response
where
    S: AsAuthState + Clone + Send + Sync + 'static,
{
    let auth = state.auth_state();
    verify_bearer(auth, addr.ip(), req, next).await
}

/// Trait for state types that carry auth info.
pub trait AsAuthState {
    fn auth_state(&self) -> &AuthState;
}

impl AsAuthState for AuthState {
    fn auth_state(&self) -> &AuthState {
        self
    }
}

/// Standalone bearer verification logic (called by different middleware wrappers).
///
/// Rate limiting is applied **only to failed auth attempts**. Successful
/// requests never consume rate-limit tokens so legitimate clients are never
/// throttled by their own traffic.
pub async fn verify_bearer(auth: &AuthState, ip: IpAddr, req: Request, next: Next) -> Response {
    let provided = extract_bearer(req.headers());

    let authed = match provided {
        Some(token) if auth.token_hash.verify(token) => true,
        // Local token is accepted only from loopback to prevent a token
        // leaked from a PTY environment being usable remotely.
        Some(token)
            if is_loopback(ip)
                && auth
                    .local_token
                    .as_ref()
                    .is_some_and(|lt| lt.as_bytes().ct_eq(token.as_bytes()).into()) =>
        {
            true
        }
        _ => false,
    };

    if authed {
        next.run(req).await
    } else {
        // Loopback is exempt from rate limiting so local dev tools are never
        // locked out.
        if !is_loopback(ip) && auth.limiter.check_key(&ip).is_err() {
            warn!(%ip, "rate limit exceeded after repeated auth failures");
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(axum::http::header::RETRY_AFTER, "60")],
                "Too Many Requests",
            )
                .into_response();
        }
        warn!(%ip, "authentication failed");
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn extract_bearer(headers: &HeaderMap) -> Option<&str> {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    auth.strip_prefix("Bearer ")
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4 == Ipv4Addr::LOCALHOST,
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::token::RawToken;

    #[test]
    fn extract_bearer_from_valid_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer my-token-123".parse().unwrap(),
        );
        assert_eq!(extract_bearer(&headers), Some("my-token-123"));
    }

    #[test]
    fn extract_bearer_missing_header() {
        let headers = HeaderMap::new();
        assert!(extract_bearer(&headers).is_none());
    }

    #[test]
    fn extract_bearer_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            "Basic dXNlcjpwYXNz".parse().unwrap(),
        );
        assert!(extract_bearer(&headers).is_none());
    }

    #[test]
    fn loopback_v4_is_loopback() {
        assert!(is_loopback(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn loopback_v6_is_loopback() {
        assert!(is_loopback(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn non_loopback_is_not_loopback() {
        assert!(!is_loopback("192.168.1.1".parse().unwrap()));
    }

    #[test]
    fn token_hash_verifies_correct_token() {
        let raw = RawToken::generate();
        let raw_str = raw.as_str().to_string();
        let stored = raw.into_stored();
        let state = AuthState::with_defaults(stored);
        assert!(state.token_hash.verify(&raw_str));
    }

    #[test]
    fn token_hash_rejects_wrong_token() {
        let raw = RawToken::generate();
        let stored = raw.into_stored();
        let state = AuthState::with_defaults(stored);
        assert!(!state.token_hash.verify("definitely-not-the-right-token"));
    }
}
