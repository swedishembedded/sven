// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that take word from the system
// around them without mistaking it for the person they serve. If your team
// needs expertise in embedding agents in a larger service then you can
// procure our services by sending an email to info@swedishembedded.com.
//! A notice: something the system hosting a session tells the conversation.
//!
//! A job finished, a file arrived, a budget ran low. The agent should know,
//! and a surface should show it, but it is not the person speaking. A notice
//! enters the conversation as a `User`-role message that says, in its first
//! line, who it comes from, and every record of the conversation (the kept
//! history, a trace, a transcript) recognises it by that line. It is never a
//! mid-conversation `System` message: a driver that keeps only the last system
//! message as the system prompt would let it replace the agent's instructions.

use serde::{Deserialize, Serialize};

/// What a trace labels the step of a notice (`extra.sven.origin`).
pub const ORIGIN: &str = "notice";

/// The longest kind, in bytes.
pub const MAX_KIND_BYTES: usize = 64;

/// The longest text, in bytes: a notice is a line of news, not a document.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

const OPEN: &str = "[host notice: ";
const ATTRIBUTION: &str =
    "] (sent by the system running this session, not by the person you are talking to)\n";

/// What the host tells a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// What sort of news this is, in the sender's own vocabulary
    /// (`job.finished`): lowercase ASCII letters, digits, `.`, `_` and `-`.
    /// sven gives no kind a meaning; surfaces and exports carry it along.
    pub kind: String,
    /// What the agent and the person are told.
    pub text: String,
}

/// Why a [`Notice`] was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeError {
    /// The kind is empty, too long, or has a character outside the set.
    Kind,
    /// The text is blank or too long.
    Text,
}

impl std::fmt::Display for NoticeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kind => write!(
                f,
                "a notice kind is 1 to {MAX_KIND_BYTES} characters of a-z, 0-9, '.', '_' and '-'"
            ),
            Self::Text => write!(
                f,
                "a notice text is not blank and at most {MAX_TEXT_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for NoticeError {}

impl Notice {
    /// A notice, if `kind` and `text` are within bounds.
    ///
    /// # Errors
    ///
    /// [`NoticeError`] naming the part that is out of bounds.
    pub fn new(kind: impl Into<String>, text: impl Into<String>) -> Result<Self, NoticeError> {
        let notice = Self {
            kind: kind.into(),
            text: text.into(),
        };
        notice.validate()?;
        Ok(notice)
    }

    /// Checks the bounds a notice that arrived over the wire must keep.
    ///
    /// # Errors
    ///
    /// [`NoticeError`] naming the part that is out of bounds.
    pub fn validate(&self) -> Result<(), NoticeError> {
        let kind_ok = !self.kind.is_empty()
            && self.kind.len() <= MAX_KIND_BYTES
            && self
                .kind
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if !kind_ok {
            return Err(NoticeError::Kind);
        }
        if self.text.trim().is_empty() || self.text.len() > MAX_TEXT_BYTES {
            return Err(NoticeError::Text);
        }
        Ok(())
    }

    /// The text of the `User`-role message the conversation holds for this
    /// notice. [`Self::parse`] reads it back.
    #[must_use]
    pub fn frame(&self) -> String {
        format!("{OPEN}{}{ATTRIBUTION}{}", self.kind, self.text)
    }

    /// The notice a conversation message framed, or `None` for any other
    /// message. A person's message that merely starts the same way is not
    /// taken for one: the whole first line must match.
    #[must_use]
    pub fn parse(message: &str) -> Option<Self> {
        let rest = message.strip_prefix(OPEN)?;
        let (kind, rest) = rest.split_once(ATTRIBUTION)?;
        let notice = Self {
            kind: kind.to_string(),
            text: rest.to_string(),
        };
        notice.validate().ok().map(|()| notice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_framed_notice_reads_back_and_says_who_sent_it() {
        let notice = Notice::new("job.finished", "Your render is ready.\nSecond line.").unwrap();
        let framed = notice.frame();
        assert!(framed.contains("not by the person you are talking to"));
        assert_eq!(Notice::parse(&framed), Some(notice));
    }

    #[test]
    fn a_persons_message_is_never_taken_for_a_notice() {
        assert_eq!(Notice::parse("hello"), None);
        assert_eq!(Notice::parse("[host notice: job.done] I typed this"), None);
        assert_eq!(
            Notice::parse(&format!("{OPEN}Bad Kind{ATTRIBUTION}x")),
            None
        );
    }

    #[test]
    fn kind_and_text_are_bounded() {
        assert_eq!(Notice::new("", "x"), Err(NoticeError::Kind));
        assert_eq!(Notice::new("Has Space", "x"), Err(NoticeError::Kind));
        assert_eq!(Notice::new("k".repeat(65), "x"), Err(NoticeError::Kind));
        assert_eq!(Notice::new("k", "  "), Err(NoticeError::Text));
        assert_eq!(
            Notice::new("k", "x".repeat(MAX_TEXT_BYTES + 1)),
            Err(NoticeError::Text)
        );
        assert!(Notice::new("k", "x".repeat(MAX_TEXT_BYTES)).is_ok());
    }
}
