// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the turn executor tells the machine about how a reply went wrong, so
//! that a harness can name the cause instead of a generic failure.
//!
//! Swedish Embedded AB implements agent runtimes that end a run with an
//! explicit, machine-readable reason for its clients. If your team needs
//! expertise in unattended agents you can procure our services by sending an
//! email to info@swedishembedded.com.

use sven_hsm::{Event, InternalEvent, CONTEXT_EXHAUSTED, TRANSIENT, TURN_TRUNCATED, TURN_WATCHDOG};
use sven_kernel::EventSink;

/// Says, just before the turn's completion, that the provider cut the reply
/// off at its output-token limit.
pub(super) async fn tell_truncated(sink: &EventSink) {
    let _ = sink
        .emit(Event::Internal(InternalEvent::Custom {
            name: TURN_TRUNCATED.to_string(),
            payload: serde_json::Value::Null,
        }))
        .await;
}

/// Says, just before the cancel that ends the turn, that a watchdog and not a
/// person cut it short, and why.
pub(super) async fn tell_watchdog(sink: &EventSink, reason: &str) {
    let _ = sink
        .emit(Event::Internal(InternalEvent::Custom {
            name: TURN_WATCHDOG.to_string(),
            payload: serde_json::Value::String(reason.to_string()),
        }))
        .await;
}

/// Words in a transport error that say the failure is of the moment: the
/// connection dropped, the server was busy or slow.
const TRANSIENT_MARKERS: [&str; 9] = [
    "connection closed",
    "connection reset",
    "connection refused",
    "error sending request",
    "timed out",
    "overloaded",
    "http 502",
    "http 503",
    "http 504",
];

/// A failed turn's error text, led by the cause when it is a known one: a
/// request that did not fit the context, or a failure that may pass.
pub(super) fn failure_text(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if sven_model::budget::is_context_overflow(error) {
        format!("{CONTEXT_EXHAUSTED}: {error}")
    } else if TRANSIENT_MARKERS.iter().any(|m| lower.contains(m)) {
        format!("{TRANSIENT}: {error}")
    } else {
        error.to_string()
    }
}
