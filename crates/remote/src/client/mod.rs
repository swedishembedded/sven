// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The HTTP side of [`sven_control::remote`]: finding a server
//! (`discovery`), the errors a caller tells apart (`errors`), a host's
//! own calls about its environment and command stream (`environment`),
//! and its calls for one conversation (`conversation`), all made through
//! one [`RemoteClient`].

use std::time::Duration;

use sven_control::remote::control_endpoint;
use uuid::Uuid;

mod conversation;
mod discovery;
mod environment;
mod errors;

pub use discovery::{discover, normalize_server};
pub(crate) use discovery::{discover_once, ensure_on_server, http, Discovery};
pub use errors::{
    Busy, ConversationNotFound, Disconnected, EnvironmentTaken, TokenRefused, UnreadableCommand,
};

/// How long a request that is not the command stream may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the bytes of one attached file may take to reach the store.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// How long the command stream may take to open, and to stay silent once
/// open. A request timeout cannot bound it (it is meant to run for minutes),
/// so these do: without them, a connection that dies without closing (a NAT
/// entry dropped, a proxy restarted, a server host gone) leaves the host
/// waiting on it for good, taking no more work.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StreamLimits {
    /// The longest wait for the response headers.
    pub open: Duration,
    /// The longest silence: the server sends a keepalive every 15 s, so four
    /// missed in a row mean the connection is gone.
    pub idle: Duration,
}

const COMMAND_STREAM: StreamLimits = StreamLimits {
    open: REQUEST_TIMEOUT,
    idle: Duration::from_secs(60),
};

/// A host's connection to its server.
#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    control_url: String,
    token: String,
    /// The environment this host attached to and the run it attached with;
    /// both named on its command stream and uploads, which the server
    /// refuses once another run holds the environment.
    attached: Option<(Uuid, Uuid)>,
    /// Whether event uploads go gzip-compressed: once the server has said it
    /// takes gzip on an answer to one (`Accept-Encoding`), never to a server
    /// too old to read one, and never again once one is refused (415).
    compress_events: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl RemoteClient {
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    pub fn new(control_url: &str, token: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: http()?,
            control_url: control_url.to_string(),
            token: token.to_string(),
            attached: None,
            compress_events: std::sync::Arc::default(),
        })
    }

    fn url(&self, endpoint: &str) -> String {
        control_endpoint(&self.control_url, endpoint)
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
