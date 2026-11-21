// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The errors a host's requests to its server end in that its caller tells
//! apart: each says what the person can do about it.

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

/// A command the server sent that this host cannot read (one of a newer
/// protocol, or malformed). Reconnecting resends it from the same place, so
/// a host that stops at it never moves on: it skips it by its `seq`.
#[derive(Debug)]
pub struct UnreadableCommand {
    /// The command's position in the stream, when the payload still says.
    pub seq: Option<u64>,
    pub(super) reason: String,
}

impl std::fmt::Display for UnreadableCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "an unreadable command ({})", self.reason)
    }
}

impl std::error::Error for UnreadableCommand {}
