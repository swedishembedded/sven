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

use sven_hsm::{Event, InternalEvent, CONTEXT_EXHAUSTED, TURN_TRUNCATED};
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

/// A failed turn's error text, led by the cause when it is a known one: a
/// request that did not fit the context.
pub(super) fn failure_text(error: &str) -> String {
    if sven_model::budget::is_context_overflow(error) {
        format!("{CONTEXT_EXHAUSTED}: {error}")
    } else {
        error.to_string()
    }
}
