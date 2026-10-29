// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for coding agents that run out of
// wall-clock time before they finish for its clients. If your team needs
// expertise in running agents under deadlines then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The shape of the line that tells a model how much time a run has left.
//!
//! The line is written by `sven-turn` and read back here by code that must
//! see a message as the person or tool wrote it: a scripted test model that
//! answers by matching the user's words must not match the clock.

/// Starts the line.
pub const TIME_NOTICE_PREFIX: &str = "Time: ";
/// Ends the line.
pub const TIME_NOTICE_SUFFIX: &str = "; leave time to verify and finish.";
/// Separates the line from the message it is appended to.
pub const TIME_NOTICE_SEPARATOR: &str = "\n\n";

/// `text` without a time notice appended at its end; `text` itself when it
/// ends with none.
pub fn without_time_notice(text: &str) -> &str {
    let Some(at) = text.rfind(TIME_NOTICE_SEPARATOR) else {
        return text;
    };
    let line = &text[at + TIME_NOTICE_SEPARATOR.len()..];
    if line.starts_with(TIME_NOTICE_PREFIX) && line.ends_with(TIME_NOTICE_SUFFIX) {
        &text[..at]
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_trailing_notice_line_is_removed() {
        let notice = format!("{TIME_NOTICE_PREFIX}1 of 9 min used, 8 min left{TIME_NOTICE_SUFFIX}");
        assert_eq!(without_time_notice(&format!("hi\n\n{notice}")), "hi");
        assert_eq!(without_time_notice("hi"), "hi");
        assert_eq!(without_time_notice("Time: a\n\nhi"), "Time: a\n\nhi");
        assert_eq!(
            without_time_notice(&format!("{notice}\n\nand more")),
            format!("{notice}\n\nand more")
        );
    }
}
