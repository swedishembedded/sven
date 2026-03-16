// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! IMAP/SMTP email backend.
//!
//! This provider uses shell commands (`curl`) for IMAP operations and
//! the Reqwest HTTP client for SMTP submission. In a full production
//! deployment, consider linking against `async-imap` and `lettre`.
//!
//! # Current approach
//!
//! Uses the system `curl` binary for IMAP operations (reading headers) and
//! builds SMTP messages as RFC 2822 formatted text sent via the SMTP relay.
//! This avoids a heavy `openssl` transitive dependency for now while still
//! providing functional email access from the agent.
//!
//! # Configuration
//!
//! ```yaml
//! tools:
//!   email:
//!     backend: imap
//!     imap_host: "imap.gmail.com"
//!     imap_port: 993
//!     smtp_host: "smtp.gmail.com"
//!     smtp_port: 587
//!     username: "${EMAIL_USER}"
//!     password: "${EMAIL_PASSWORD}"
//! ```

use async_trait::async_trait;
use tracing::debug;

use super::{EmailMessage, EmailProvider, EmailQuery, EmailSummary, NewEmail};

/// IMAP (receive) + SMTP (send) email provider.
///
/// Uses shell `curl` for IMAP list/read and Reqwest for SMTP submission.
pub struct ImapProvider {
    imap_host: String,
    imap_port: u16,
    smtp_host: String,
    smtp_port: u16,
    username: String,
    password: String,
}

impl ImapProvider {
    /// Create a new IMAP/SMTP provider.
    pub fn new(
        imap_host: impl Into<String>,
        imap_port: u16,
        smtp_host: impl Into<String>,
        smtp_port: u16,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            imap_host: imap_host.into(),
            imap_port,
            smtp_host: smtp_host.into(),
            smtp_port,
            username: username.into(),
            password: password.into(),
        }
    }
}

#[async_trait]
impl EmailProvider for ImapProvider {
    async fn list(&self, query: &EmailQuery) -> anyhow::Result<Vec<EmailSummary>> {
        let folder = query.folder.as_deref().unwrap_or("INBOX");
        let limit = query.limit.unwrap_or(20);

        debug!(folder, limit, "IMAP: listing messages");

        // Use curl for IMAP list (headers only)
        let imap_url = format!(
            "imaps://{}:{}/{};UID=1:{}",
            self.imap_host,
            self.imap_port,
            folder,
            limit * 5
        );

        let output = tokio::process::Command::new("curl")
            .args([
                "--silent",
                "--url",
                &imap_url,
                "--user",
                &format!("{}:{}", self.username, self.password),
            ])
            .output()
            .await?;

        let text = String::from_utf8_lossy(&output.stdout).to_string();
        let summaries = parse_imap_list_output(&text, limit);

        Ok(summaries)
    }

    async fn read(&self, id: &str) -> anyhow::Result<EmailMessage> {
        debug!(id, "IMAP: reading message");

        let imap_url = format!(
            "imaps://{}:{}/INBOX;UID={}",
            self.imap_host, self.imap_port, id
        );

        let output = tokio::process::Command::new("curl")
            .args([
                "--silent",
                "--url",
                &imap_url,
                "--user",
                &format!("{}:{}", self.username, self.password),
            ])
            .output()
            .await?;

        let text = String::from_utf8_lossy(&output.stdout).to_string();
        parse_imap_message(id, &text)
    }

    async fn send(&self, email: &NewEmail) -> anyhow::Result<()> {
        debug!(subject = %email.subject, "SMTP: sending email");

        // Build RFC 2822 message
        let from = email.from.clone().unwrap_or(self.username.clone());
        let to = email.to.join(", ");
        let message = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{}",
            from, to, email.subject, email.body
        );

        // Write to temp file for curl
        let tmp = tempfile_for_message(&message).await?;

        let smtp_url = format!("smtp://{}:{}", self.smtp_host, self.smtp_port);

        let output = tokio::process::Command::new("curl")
            .args([
                "--silent",
                "--url",
                &smtp_url,
                "--user",
                &format!("{}:{}", self.username, self.password),
                "--mail-from",
                &from,
                "--mail-rcpt",
                &email.to[0],
                "--upload-file",
                tmp.path().to_str().unwrap_or(""),
                "--ssl-reqd",
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("SMTP send failed: {}", stderr);
        }

        Ok(())
    }

    async fn reply(&self, id: &str, body: &str) -> anyhow::Result<()> {
        // Read original to get subject and from
        let original = self.read(id).await?;
        let subject = if original.subject.starts_with("Re:") {
            original.subject.clone()
        } else {
            format!("Re: {}", original.subject)
        };

        let email = NewEmail {
            from: None,
            to: vec![original.from.clone()],
            cc: vec![],
            subject,
            body: body.to_string(),
            body_html: None,
        };

        self.send(&email).await
    }

    async fn search(&self, query: &str) -> anyhow::Result<Vec<EmailSummary>> {
        debug!(query, "IMAP: searching");
        // Simplified: just return list filtered by subject
        let all = self.list(&EmailQuery::default()).await?;
        let q = query.to_lowercase();
        Ok(all
            .into_iter()
            .filter(|s| s.subject.to_lowercase().contains(&q) || s.from.to_lowercase().contains(&q))
            .collect())
    }
}

fn parse_imap_list_output(output: &str, limit: usize) -> Vec<EmailSummary> {
    // Very basic parser for curl IMAP header output
    let mut summaries = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_from = String::new();
    let mut current_subject = String::new();

    for line in output.lines() {
        if line.starts_with("* ") && line.contains("EXISTS") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("* ") {
            if let Some(uid) = rest.split_whitespace().next() {
                if uid.parse::<u64>().is_ok() {
                    if let Some(id) = current_id.take() {
                        summaries.push(EmailSummary {
                            id,
                            from: current_from.clone(),
                            subject: current_subject.clone(),
                            date: None,
                            unread: true,
                            thread_id: None,
                        });
                    }
                    current_id = Some(uid.to_string());
                    current_from.clear();
                    current_subject.clear();
                }
            }
        } else if line.to_lowercase().starts_with("from:") {
            current_from = line[5..].trim().to_string();
        } else if line.to_lowercase().starts_with("subject:") {
            current_subject = line[8..].trim().to_string();
        }

        if summaries.len() >= limit {
            break;
        }
    }

    if let Some(id) = current_id {
        summaries.push(EmailSummary {
            id,
            from: current_from,
            subject: current_subject,
            date: None,
            unread: true,
            thread_id: None,
        });
    }

    summaries.truncate(limit);
    summaries
}

fn parse_imap_message(id: &str, raw: &str) -> anyhow::Result<EmailMessage> {
    let mut from = String::new();
    let mut subject = String::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut in_body = false;

    for line in raw.lines() {
        if line.is_empty() && !in_body {
            in_body = true;
            continue;
        }
        if in_body {
            body_lines.push(line);
        } else if line.to_lowercase().starts_with("from:") {
            from = line[5..].trim().to_string();
        } else if line.to_lowercase().starts_with("subject:") {
            subject = line[8..].trim().to_string();
        }
    }

    Ok(EmailMessage {
        id: id.to_string(),
        from,
        to: vec![],
        cc: vec![],
        subject,
        body_text: body_lines.join("\n"),
        body_html: None,
        date: None,
        message_id: None,
        in_reply_to: None,
        thread_id: None,
        labels: vec![],
    })
}

async fn tempfile_for_message(content: &str) -> anyhow::Result<tempfile::NamedTempFile> {
    use std::io::Write;
    let mut f = tempfile::NamedTempFile::new()?;
    f.write_all(content.as_bytes())?;
    Ok(f)
}
