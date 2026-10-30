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
/// connection dropped, the server was busy or slow, the provider broke a
/// generation off (`finish_reason: error`, which OpenRouter sends when the
/// upstream provider fails mid-stream), a server that had accepted the
/// request sent an error frame instead of the reply (`server error`, the
/// stream parser's prefix: an aborted, refused or failed upstream
/// completion, whatever its wording), or the stream went silent past the
/// stall watchdog's idle limit. A request too large for the context is
/// classified before these; a content-filter stop by the model itself is not
/// one.
const TRANSIENT_MARKERS: [&str; 12] = [
    "stream stopped: error",
    "stale connection",
    "connection closed",
    "connection reset",
    "connection refused",
    "error sending request",
    "timed out",
    "overloaded",
    "http 502",
    "http 503",
    "http 504",
    "server error",
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

#[cfg(test)]
mod tests {
    use super::failure_text;
    use sven_hsm::TRANSIENT;

    #[test]
    fn a_generation_the_provider_broke_off_is_retried_and_a_filtered_one_is_not() {
        let broken = failure_text("model completion failed: stream stopped: error");
        assert!(broken.starts_with(TRANSIENT), "{broken}");
        assert!(failure_text("HTTP 503 Service Unavailable").starts_with(TRANSIENT));
        let stale = failure_text("model stream idle for >300 s - stale connection");
        assert!(stale.starts_with(TRANSIENT), "{stale}");
        let filtered = failure_text("stream stopped: content_filter");
        assert!(!filtered.starts_with(TRANSIENT), "{filtered}");
    }

    #[test]
    fn an_error_frame_sent_mid_stream_is_retried_whatever_its_wording() {
        // OpenRouter relays an upstream provider's failure or refusal of one
        // completion as an error frame, in many wordings; the next request
        // may be served (or routed) otherwise.
        let upstream = failure_text(
            "model completion failed: server error: Upstream error from Alibaba: \
             Output data may contain inappropriate content.",
        );
        assert!(upstream.starts_with(TRANSIENT), "{upstream}");
        let relayed =
            failure_text("model completion failed: server error: Provider returned error");
        assert!(relayed.starts_with(TRANSIENT), "{relayed}");
        let aborted =
            failure_text("model completion failed: server error: The operation was aborted");
        assert!(aborted.starts_with(TRANSIENT), "{aborted}");
        let coded = failure_text("server error (502): Bad gateway upstream");
        assert!(coded.starts_with(TRANSIENT), "{coded}");
    }
}
