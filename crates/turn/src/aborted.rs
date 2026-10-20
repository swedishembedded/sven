// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The marker error of a turn that was cut short on purpose.

/// Marker error distinguishing a deliberate abort - a user cancel (Esc,
/// Ctrl+C, `/abort`, ACP `cancel()`) or the thinking-token/time watchdog -
/// from a genuine turn failure. `TurnExecutor` wraps its own cancel-channel
/// trigger in this, and [`crate::stream_turn::stream_turn`]'s watchdog check does the same, so a
/// single `downcast_ref::<AbortedError>()` after the turn's `tokio::select!`
/// collapses both onto `Event::UserCancelled`/`UiEvent::Aborted` instead of
/// `Event::LlmFailed`/`UiEvent::Error` - the latter drives `SdlcMachine` into
/// `Recovery`/`Failed`, which is correct for a real failure but wrong for a
/// turn the user (or the watchdog) deliberately cut short.
#[derive(Debug)]
pub struct AbortedError {
    /// What cut the turn short, as shown to the user.
    pub reason: String,
    /// Whether a person asked for it. A watchdog abort is not: it is a
    /// failure of the turn that a consumer must be told about.
    pub by_user: bool,
}

impl AbortedError {
    /// A cancel a person asked for (Esc, Ctrl+C, `/abort`, ACP `cancel()`).
    #[must_use]
    pub fn by_user(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            by_user: true,
        }
    }

    /// A cut-off the thinking watchdog decided on.
    #[must_use]
    pub fn by_watchdog(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            by_user: false,
        }
    }
}

impl std::fmt::Display for AbortedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}

impl std::error::Error for AbortedError {}
