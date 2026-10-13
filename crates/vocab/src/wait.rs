// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for silent, hung model connections
// for its clients. If your team needs expertise in observable agent runtimes
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! What a turn is waiting for while the model has produced nothing real.

use serde::{Deserialize, Serialize};

/// Where a streamed model turn is stuck waiting, as reported by
/// [`SessionEvent::ModelWaiting`](crate::SessionEvent::ModelWaiting).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelWaitPhase {
    /// The request is sent and the server has not answered with response
    /// headers yet.
    Connecting,
    /// The server answered, but no text, reasoning or tool-call chunk has
    /// arrived yet. Keepalives do not end this phase.
    AwaitingFirstToken,
    /// The model was producing output and has gone quiet.
    Idle,
}

impl std::fmt::Display for ModelWaitPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Connecting => "connecting",
            Self::AwaitingFirstToken => "awaiting first token",
            Self::Idle => "idle",
        })
    }
}
