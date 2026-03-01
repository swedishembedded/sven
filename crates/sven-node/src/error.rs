// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("TLS error: {0}")]
    Tls(String),

    #[error("HTTP server error: {0}")]
    Http(#[from] std::io::Error),

    #[error("P2P error: {0}")]
    P2p(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("peer not authorized: {0}")]
    NotAuthorized(String),

    #[error("rate limited")]
    RateLimited,

    #[error("Slack signature verification failed: {0}")]
    SlackVerify(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("serialization error: {0}")]
    Serde(String),
}
