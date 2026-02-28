// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//!
//! Security headers and CSRF protection middleware.
//!
//! # Headers applied to every response
//!
//! | Header                         | Value                             |
//! |--------------------------------|-----------------------------------|
//! | `Strict-Transport-Security`    | `max-age=31536000; includeSubDomains` |
//! | `X-Content-Type-Options`       | `nosniff`                         |
//! | `X-Frame-Options`              | `DENY`                            |
//! | `Referrer-Policy`              | `no-referrer`                     |
//! | `Permissions-Policy`           | camera/mic/geolocation disabled   |
//! | `Content-Security-Policy`      | strict, no inline scripts         |
//!
//! HSTS is set even though the gateway defaults to loopback-only. If the
//! operator exposes it over LAN or Tailscale the header will already be there.
//!
//! # CSRF protection
//!
//! Cross-origin mutating requests (POST/PUT/PATCH/DELETE) are rejected by
//! inspecting `Origin`, `Referer`, and `Sec-Fetch-Site` headers. This is
//! the same defence-in-depth approach as openclaw, but applied to all routes
//! rather than only the control UI.
//!
//! WebSocket upgrade requests are exempt (browsers enforce same-origin for WS).

use axum::{
    extract::Request,
    http::{HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

// ── Constant header values ────────────────────────────────────────────────────

static HSTS: HeaderValue = HeaderValue::from_static("max-age=31536000; includeSubDomains");
static NO_SNIFF: HeaderValue = HeaderValue::from_static("nosniff");
static DENY_FRAME: HeaderValue = HeaderValue::from_static("DENY");
static NO_REFERRER: HeaderValue = HeaderValue::from_static("no-referrer");
static PERMISSIONS: HeaderValue =
    HeaderValue::from_static("camera=(), microphone=(), geolocation=()");
static CSP: HeaderValue = HeaderValue::from_static(
    "default-src 'self'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' data:; \
     connect-src 'self' wss: ws:; \
     frame-ancestors 'none'; \
     base-uri 'none'; \
     object-src 'none'",
);

// ── Middleware ────────────────────────────────────────────────────────────────

/// Append security headers to every outgoing response.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(axum::http::header::STRICT_TRANSPORT_SECURITY, HSTS.clone());
    h.insert(axum::http::header::X_CONTENT_TYPE_OPTIONS, NO_SNIFF.clone());
    h.insert(axum::http::header::X_FRAME_OPTIONS, DENY_FRAME.clone());
    h.insert(axum::http::header::REFERRER_POLICY, NO_REFERRER.clone());
    h.insert("permissions-policy", PERMISSIONS.clone());
    h.insert(axum::http::header::CONTENT_SECURITY_POLICY, CSP.clone());
    resp
}

/// Reject cross-origin mutating requests (CSRF protection).
///
/// Only checks POST/PUT/PATCH/DELETE. GET, HEAD, OPTIONS, and WebSocket
/// upgrades are passed through.
pub async fn csrf_guard(req: Request, next: Next) -> Response {
    // WebSocket upgrades are exempt.
    let is_ws_upgrade = req
        .headers()
        .get(axum::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if !is_ws_upgrade && is_mutating_method(req.method()) {
        if let Some(reason) = should_reject_cross_origin(req.headers()) {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
    }

    next.run(req).await
}

// ── Internal logic ────────────────────────────────────────────────────────────

fn is_mutating_method(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Returns `Some(reason)` if the request should be rejected as cross-origin.
fn should_reject_cross_origin(headers: &axum::http::HeaderMap) -> Option<&'static str> {
    // Strongest signal: `Sec-Fetch-Site` is set by modern browsers.
    if let Some(sfs) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        if sfs.eq_ignore_ascii_case("cross-site") {
            return Some("Forbidden: cross-site request");
        }
        // same-site and same-origin are fine.
        return None;
    }

    // Fall back to Origin header.
    if let Some(origin) = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if !is_loopback_origin(origin) {
            return Some("Forbidden: cross-origin request");
        }
        return None;
    }

    // Fall back to Referer header.
    if let Some(referer) = headers
        .get(axum::http::header::REFERER)
        .and_then(|v| v.to_str().ok())
    {
        if !is_loopback_origin(referer) {
            return Some("Forbidden: cross-origin referer");
        }
    }

    // Non-browser clients (curl, reqwest, native apps) typically send no
    // Origin/Referer. Allow them through — they cannot be a browser-based
    // CSRF attack vector.
    None
}

fn is_loopback_origin(url: &str) -> bool {
    // Accept anything pointing to 127.x.x.x, localhost, or ::1.
    url.contains("localhost") || url.contains("127.0.0.") || url.contains("[::1]")
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers_with(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                axum::http::header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        m
    }

    #[test]
    fn cross_site_sec_fetch_site_is_rejected() {
        let h = headers_with(&[("sec-fetch-site", "cross-site")]);
        assert!(should_reject_cross_origin(&h).is_some());
    }

    #[test]
    fn same_origin_sec_fetch_site_is_allowed() {
        let h = headers_with(&[("sec-fetch-site", "same-origin")]);
        assert!(should_reject_cross_origin(&h).is_none());
    }

    #[test]
    fn cross_origin_header_is_rejected() {
        let h = headers_with(&[("origin", "https://evil.com")]);
        assert!(should_reject_cross_origin(&h).is_some());
    }

    #[test]
    fn localhost_origin_is_allowed() {
        let h = headers_with(&[("origin", "http://localhost:18790")]);
        assert!(should_reject_cross_origin(&h).is_none());
    }

    #[test]
    fn loopback_ip_origin_is_allowed() {
        let h = headers_with(&[("origin", "http://127.0.0.1:18790")]);
        assert!(should_reject_cross_origin(&h).is_none());
    }

    #[test]
    fn no_origin_no_referer_is_allowed() {
        let h = HeaderMap::new();
        assert!(should_reject_cross_origin(&h).is_none());
    }

    #[test]
    fn post_is_mutating() {
        assert!(is_mutating_method(&Method::POST));
        assert!(is_mutating_method(&Method::PUT));
        assert!(is_mutating_method(&Method::DELETE));
    }

    #[test]
    fn get_is_not_mutating() {
        assert!(!is_mutating_method(&Method::GET));
        assert!(!is_mutating_method(&Method::HEAD));
    }
}
