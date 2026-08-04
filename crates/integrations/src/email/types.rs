// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Email message types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Query parameters for listing / filtering emails.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EmailQuery {
    /// Mailbox folder to query (default: INBOX).
    pub folder: Option<String>,
    /// Maximum number of messages to return (default: 20).
    pub limit: Option<usize>,
    /// Only return unread messages.
    pub unread_only: bool,
    /// Sender address filter.
    pub from: Option<String>,
    /// Subject substring filter.
    pub subject: Option<String>,
    /// Return messages received after this timestamp.
    pub since: Option<DateTime<Utc>>,
}

/// Brief summary of an email (for listing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSummary {
    /// Provider-specific message identifier.
    pub id: String,
    /// From address.
    pub from: String,
    /// Subject line.
    pub subject: String,
    /// Date received.
    pub date: Option<DateTime<Utc>>,
    /// True if the message has not been read.
    pub unread: bool,
    /// Message thread / conversation ID (provider-specific).
    pub thread_id: Option<String>,
}

/// Full email message with headers and body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailMessage {
    /// Provider-specific message identifier.
    pub id: String,
    /// From address.
    pub from: String,
    /// To addresses.
    pub to: Vec<String>,
    /// CC addresses.
    pub cc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain text body.
    pub body_text: String,
    /// HTML body (if available).
    pub body_html: Option<String>,
    /// Date received.
    pub date: Option<DateTime<Utc>>,
    /// Message-ID header for reply threading.
    pub message_id: Option<String>,
    /// In-Reply-To header for chaining.
    pub in_reply_to: Option<String>,
    /// Provider-specific thread ID.
    pub thread_id: Option<String>,
    /// Labels or folder name.
    pub labels: Vec<String>,
}

/// A new outbound email.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewEmail {
    /// From address (uses account default if None).
    pub from: Option<String>,
    /// To addresses.
    pub to: Vec<String>,
    /// CC addresses.
    pub cc: Vec<String>,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub body: String,
    /// HTML body (optional).
    pub body_html: Option<String>,
}

impl NewEmail {
    /// Convenience constructor for a simple text email.
    pub fn simple(
        to: impl Into<String>,
        subject: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            from: None,
            to: vec![to.into()],
            cc: vec![],
            subject: subject.into(),
            body: body.into(),
            body_html: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_email_simple_sets_fields() {
        let email = NewEmail::simple("bob@example.com", "Hello", "Hi Bob");
        assert_eq!(email.to, vec!["bob@example.com"]);
        assert_eq!(email.subject, "Hello");
        assert_eq!(email.body, "Hi Bob");
        assert!(email.from.is_none());
        assert!(email.cc.is_empty());
        assert!(email.body_html.is_none());
    }

    #[test]
    fn email_query_default_is_empty() {
        let q = EmailQuery::default();
        assert!(q.folder.is_none());
        assert!(!q.unread_only);
    }

    #[test]
    fn email_summary_roundtrips_json() {
        let summary = EmailSummary {
            id: "msg-1".to_string(),
            from: "alice@example.com".to_string(),
            subject: "Test".to_string(),
            date: None,
            unread: true,
            thread_id: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let decoded: EmailSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, "msg-1");
        assert!(decoded.unread);
    }

    #[test]
    fn new_email_roundtrips_json() {
        let email = NewEmail::simple("to@example.com", "Subj", "Body text");
        let json = serde_json::to_string(&email).unwrap();
        let decoded: NewEmail = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.subject, "Subj");
        assert_eq!(decoded.body, "Body text");
    }
}
