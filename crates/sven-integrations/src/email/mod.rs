// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Email integration — IMAP/SMTP and Gmail API.
//!
//! # Providers
//!
//! | Provider | Auth | Notes |
//! |----------|------|-------|
//! | [`ImapProvider`] | Username + password | Works with any IMAP/SMTP server |
//! | [`GmailProvider`] | OAuth2 | Rich Gmail-specific features (labels, threads) |
//!
//! # Tool
//!
//! [`EmailTool`] wraps any provider and exposes: `list`, `read`, `send`, `reply`, `search`.

pub mod gmail;
pub mod imap;
pub mod tool;
pub mod types;

pub use gmail::GmailProvider;
pub use imap::ImapProvider;
pub use tool::EmailTool;
pub use types::{EmailMessage, EmailQuery, EmailSummary, NewEmail};

use async_trait::async_trait;

/// Unified email provider trait.
///
/// Implement this trait to add a new email backend.
#[async_trait]
pub trait EmailProvider: Send + Sync {
    /// List messages matching a query.
    async fn list(&self, query: &EmailQuery) -> anyhow::Result<Vec<EmailSummary>>;

    /// Read the full content of a message by ID.
    async fn read(&self, id: &str) -> anyhow::Result<EmailMessage>;

    /// Send a new email.
    async fn send(&self, email: &NewEmail) -> anyhow::Result<()>;

    /// Reply to a message by ID.
    async fn reply(&self, id: &str, body: &str) -> anyhow::Result<()>;

    /// Search for messages using a query string.
    ///
    /// The query syntax depends on the backend (IMAP search, Gmail query, etc.)
    async fn search(&self, query: &str) -> anyhow::Result<Vec<EmailSummary>>;
}
