// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Options for the one-tap `/share` command.
//!
//! This is pure data: the `/share` builtin ([`crate::builtin::share`]) builds
//! it from the environment, and `sven-frontend`'s `run_frontend_share_bridge`
//! (`sven_frontend::share`) is what actually dials the broker and drives the
//! WebSocket bridge. The struct lives here — one tier below `sven-frontend`
//! — rather than in `sven-frontend::share` because the command that
//! constructs it now lives in this crate; `sven-frontend` re-exports it at
//! its historical path (`sven_frontend::FrontendShareOptions`) for existing
//! callers.

use std::path::PathBuf;

/// How to reach the broker and announce the shared session.
#[derive(Debug, Clone)]
pub struct FrontendShareOptions {
    /// Broker base URL (e.g. `https://localhost:8443`) or an explicit
    /// `wss://…/share` URL. `https`/`http` are rewritten to `wss`/`ws` and
    /// `/share` is appended when absent.
    pub broker_url: String,
    /// Tenant bearer secret (any valid token of the tenant). The broker takes
    /// the tenant from this token.
    pub token: String,
    /// Stable id a consultant attaches by (`GET /share/:share_id`).
    pub share_id: String,
    /// Tenant that owns the share. Must match the token's tenant or the broker
    /// closes the connection.
    pub tenant_id: String,
    /// Human-readable label shown to the consultant.
    pub title: String,
    /// Extra CA PEM to trust (self-signed control planes).
    pub ca_pem: Option<PathBuf>,
    /// Disable TLS verification — local testing only.
    pub insecure_dev: bool,
}
