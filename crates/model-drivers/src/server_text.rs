// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that report what a
// model service said without letting the service write to their terminal,
// for its clients. If your team needs expertise in multi-provider LLM
// integration then you can procure our services by sending an email to
// info@swedishembedded.com.
//! What a server said, made safe to show: the text of a refused request or a
//! stream's error goes into an error message that a terminal prints.
//!
//! A server's body is not trusted to be text. A compressed body that arrived
//! without its encoding is binary, and any body may hold control characters,
//! terminal escape sequences among them. [`printable`] removes every control
//! character but line breaks and tabs, and says how long a body was when it
//! is mostly not text. Text is otherwise kept whole: callers recognise some
//! refusals by what they say.

/// `text` as it may be shown: without control characters (line breaks and
/// tabs stay), or a note of its length when most of it is not text.
#[must_use]
pub fn printable(text: &str) -> String {
    let total = text.chars().count();
    let unreadable = text.chars().filter(|c| is_unreadable(*c)).count();
    if total > 0 && unreadable * 4 > total {
        return format!("<{} bytes that are not text>", text.len());
    }
    text.chars().filter(|c| !is_unreadable(*c)).collect()
}

/// A control character other than a line break or a tab, or the character a
/// lossy decoding put where the bytes were not UTF-8.
fn is_unreadable(c: char) -> bool {
    (c.is_control() && !matches!(c, '\n' | '\r' | '\t')) || c == char::REPLACEMENT_CHARACTER
}

#[cfg(test)]
mod tests {
    use super::printable;

    #[test]
    fn a_servers_message_is_kept_whole() {
        let body = "{\"error\":{\"message\":\"Unauthorized\",\"code\":\"context_length_exceeded\"}}\nretry later\t(2)";
        assert_eq!(printable(body), body);
        assert_eq!(printable(""), "");
    }

    #[test]
    fn a_compressed_body_is_named_not_printed() {
        // gzip's header and deflated bytes, as a lossy UTF-8 decoding makes them.
        let bytes = [
            0x1f, 0x8b, 0x08, 0, 0, 0, 0, 0, 0x02, 0xff, 0x4b, 0xcd, 0xa8, 0x56, 0x4a, 0xce, 0xc8,
            0x8f, 0x4c,
        ];
        let body = String::from_utf8_lossy(&bytes);
        assert_eq!(
            printable(&body),
            format!("<{} bytes that are not text>", body.len())
        );
    }

    #[test]
    fn control_characters_never_reach_the_terminal() {
        let body = "Unauthorized\u{1b}]0;owned\u{7}\u{1b}[2J: key rejected\u{0}";
        let shown = printable(body);
        assert_eq!(shown, "Unauthorized]0;owned[2J: key rejected");
        assert!(!shown.chars().any(|c| c.is_control()));
    }
}
