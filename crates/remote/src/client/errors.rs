// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The errors a host's request to the sven service ends with that a caller
//! acts on: each names one reason the server gave, so the caller can tell a
//! token to renew from an environment someone else holds.

use uuid::Uuid;

/// The server no longer accepts the host's token: revoked, or never valid.
/// Retrying cannot help; the person signs in again.
#[derive(Debug)]
pub struct TokenRefused;

impl std::fmt::Display for TokenRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the server does not accept this host's token; sign in again with `sven auth login`",
        )
    }
}

impl std::error::Error for TokenRefused {}

/// Another live `sven rc` holds the environment of this directory, or took
/// it over from this one, which must stop; the reason is the server's.
#[derive(Debug)]
pub struct EnvironmentTaken(pub String);

impl std::fmt::Display for EnvironmentTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EnvironmentTaken {}

/// The server's reason for a 409 on a host's request: another run holds, or
/// took over, the environment.
pub(super) async fn superseded(response: reqwest::Response) -> EnvironmentTaken {
    EnvironmentTaken(
        response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "another sven rc holds this environment".into()),
    )
}

/// The server knows no conversation by this id that runs on this host.
#[derive(Debug)]
pub struct ConversationNotFound(pub Uuid);

impl std::fmt::Display for ConversationNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no conversation {} runs on this machine", self.0)
    }
}

impl std::error::Error for ConversationNotFound {}

/// The server did not take the message now (an answer is being written);
/// the reason is the server's.
#[derive(Debug)]
pub struct Busy(pub String);

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Busy {}

/// The machine the conversation runs on is not connected, so the server
/// stored nothing; the reason says where to run `sven rc` to reconnect it.
#[derive(Debug)]
pub struct Disconnected(pub String);

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Disconnected {}
