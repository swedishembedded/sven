// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that take a whole plan as one
// message for its clients. If your team needs expertise in agent input
// handling then you can procure our services by sending an email to
// info@swedishembedded.com.
//! How much a person may send as one message's text.
//!
//! A long plan pasted as a message goes straight into the conversation, so
//! the agent acts on it at once: up to [`MAX_MESSAGE_BYTES`] of it. Longer
//! text travels as a file the message attaches, which the agent reads when it
//! needs to. Every client applies this one limit: the terminal attaches a
//! longer paste, a server that sends a session its messages (`sven rc`) keeps
//! to it, and the host refuses a message over it rather than passing it on.
//! Whether a message fits the model is a separate gate, per model
//! (`sven_model::budget`).

/// The longest text of one message, in bytes of UTF-8 (100 KiB). Bytes, not
/// characters: every client counts them the same way.
pub const MAX_MESSAGE_BYTES: usize = 100 * 1024;

/// Whether `text` may be sent as one message's text.
#[must_use]
pub fn fits_message(text: &str) -> bool {
    text.len() <= MAX_MESSAGE_BYTES
}

/// What a person is told when a message is over the limit: its size, the
/// limit, and what to do instead.
#[must_use]
pub fn too_long(bytes: usize) -> String {
    format!(
        "This message is {} KB; a message may be at most {} KB. Attach longer text as a file.",
        bytes.div_ceil(1024),
        MAX_MESSAGE_BYTES / 1024
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hundred_kilobytes_fit_and_one_byte_more_does_not() {
        assert!(fits_message(&"a".repeat(MAX_MESSAGE_BYTES)));
        assert!(!fits_message(&"a".repeat(MAX_MESSAGE_BYTES + 1)));
        // Counted in bytes: a letter of two bytes counts twice.
        assert!(!fits_message(&"ö".repeat(MAX_MESSAGE_BYTES / 2 + 1)));
    }

    #[test]
    fn the_refusal_names_the_size_the_limit_and_the_way_out() {
        assert_eq!(
            too_long(150 * 1024),
            "This message is 150 KB; a message may be at most 100 KB. Attach longer text as a file."
        );
    }
}
