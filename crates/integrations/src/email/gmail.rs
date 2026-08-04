// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Gmail REST API email backend (OAuth2).
//!
//! Uses the Gmail API v1 with OAuth2 access tokens stored in a JSON file.
//!
//! # Setup
//!
//! 1. Create a Google Cloud project and enable the Gmail API.
//! 2. Create OAuth2 credentials (Desktop application type).
//! 3. Run the OAuth2 authorization flow once to obtain tokens.
//! 4. Configure sven:
//!    ```yaml
//!    tools:
//!      email:
//!        backend: gmail
//!        oauth_client_id: "${GMAIL_CLIENT_ID}"
//!        oauth_client_secret: "${GMAIL_CLIENT_SECRET}"
//!        oauth_token_path: "~/.config/sven/gmail-token.json"
//!    ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::debug;

use super::{EmailMessage, EmailProvider, EmailQuery, EmailSummary, NewEmail};

/// Gmail API token storage.
#[derive(Debug, Serialize, Deserialize)]
struct GmailToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

/// Gmail REST API email provider.
pub struct GmailProvider {
    /// Stored for OAuth2 token refresh flows.
    #[allow(dead_code)]
    client_id: String,
    /// Stored for OAuth2 token refresh flows.
    #[allow(dead_code)]
    client_secret: String,
    token_path: PathBuf,
    client: reqwest::Client,
}

impl GmailProvider {
    const GMAIL_API: &'static str = "https://gmail.googleapis.com/gmail/v1/users/me";

    /// Create a new Gmail provider.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        token_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            token_path: token_path.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Gmail HTTP client"),
        }
    }

    async fn access_token(&self) -> anyhow::Result<String> {
        let text = tokio::fs::read_to_string(&self.token_path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Gmail token file not found at {}. \
                 Run the OAuth2 setup flow first: {e}",
                    self.token_path.display()
                )
            })?;
        let token: GmailToken = serde_json::from_str(&text)?;
        // TODO: auto-refresh when expired
        Ok(token.access_token)
    }

    async fn get_json(&self, url: &str) -> anyhow::Result<serde_json::Value> {
        let token = self.access_token().await?;
        Ok(self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await?
            .json()
            .await?)
    }
}

#[async_trait]
impl EmailProvider for GmailProvider {
    async fn list(&self, query: &EmailQuery) -> anyhow::Result<Vec<EmailSummary>> {
        let limit = query.limit.unwrap_or(20);
        debug!(limit, "Gmail: listing messages");

        let mut gmail_query = String::new();
        if query.unread_only {
            gmail_query.push_str("is:unread ");
        }
        if let Some(f) = &query.from {
            gmail_query.push_str(&format!("from:{f} "));
        }
        if let Some(s) = &query.subject {
            gmail_query.push_str(&format!("subject:{s} "));
        }
        if let Some(since) = &query.since {
            gmail_query.push_str(&format!("after:{} ", since.format("%Y/%m/%d")));
        }

        let url = format!(
            "{}/messages?maxResults={}&q={}",
            Self::GMAIL_API,
            limit,
            urlencoding(&gmail_query)
        );

        let resp = self.get_json(&url).await?;
        let message_refs = resp["messages"].as_array().cloned().unwrap_or_default();

        let mut summaries = Vec::new();
        for msg_ref in message_refs.iter().take(limit) {
            let id = match msg_ref["id"].as_str() {
                Some(id) => id.to_string(),
                None => continue,
            };

            let meta_url = format!("{}/messages/{}?format=metadata&metadataHeaders=From&metadataHeaders=Subject&metadataHeaders=Date", Self::GMAIL_API, id);
            let meta = self.get_json(&meta_url).await.unwrap_or_default();

            let headers = meta["payload"]["headers"].as_array();
            let from = extract_header(headers, "From");
            let subject = extract_header(headers, "Subject");
            let unread = meta["labelIds"]
                .as_array()
                .map(|labels| labels.iter().any(|l| l.as_str() == Some("UNREAD")))
                .unwrap_or(false);
            let thread_id = meta["threadId"].as_str().map(|s| s.to_string());

            summaries.push(EmailSummary {
                id,
                from,
                subject,
                date: None,
                unread,
                thread_id,
            });
        }

        Ok(summaries)
    }

    async fn read(&self, id: &str) -> anyhow::Result<EmailMessage> {
        debug!(id, "Gmail: reading message");

        let url = format!("{}/messages/{}?format=full", Self::GMAIL_API, id);
        let msg = self.get_json(&url).await?;

        let headers = msg["payload"]["headers"].as_array();
        let from = extract_header(headers, "From");
        let subject = extract_header(headers, "Subject");
        let to_str = extract_header(headers, "To");
        let message_id = extract_header_opt(headers, "Message-ID");

        let body_text = extract_body(&msg["payload"], "text/plain");
        let body_html = {
            let h = extract_body(&msg["payload"], "text/html");
            if h.is_empty() {
                None
            } else {
                Some(h)
            }
        };

        let labels: Vec<String> = msg["labelIds"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        Ok(EmailMessage {
            id: id.to_string(),
            from,
            to: to_str.split(',').map(|s| s.trim().to_string()).collect(),
            cc: vec![],
            subject,
            body_text,
            body_html,
            date: None,
            message_id,
            in_reply_to: None,
            thread_id: msg["threadId"].as_str().map(|s| s.to_string()),
            labels,
        })
    }

    async fn send(&self, email: &NewEmail) -> anyhow::Result<()> {
        debug!(subject = %email.subject, "Gmail: sending");
        let token = self.access_token().await?;

        // Build RFC 2822 raw message
        let raw = build_raw_message(email);
        let encoded = base64_url_encode(raw.as_bytes());

        let payload = serde_json::json!({ "raw": encoded });

        self.client
            .post(format!("{}/messages/send", Self::GMAIL_API))
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?;

        Ok(())
    }

    async fn reply(&self, id: &str, body: &str) -> anyhow::Result<()> {
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
        self.list(&EmailQuery {
            subject: Some(query.to_string()),
            ..Default::default()
        })
        .await
    }
}

fn extract_header(headers: Option<&Vec<serde_json::Value>>, name: &str) -> String {
    extract_header_opt(headers, name).unwrap_or_default()
}

fn extract_header_opt(headers: Option<&Vec<serde_json::Value>>, name: &str) -> Option<String> {
    headers?.iter().find_map(|h| {
        if h["name"].as_str()?.eq_ignore_ascii_case(name) {
            h["value"].as_str().map(|s| s.to_string())
        } else {
            None
        }
    })
}

fn extract_body(payload: &serde_json::Value, mime_type: &str) -> String {
    // Direct body
    if payload["mimeType"].as_str() == Some(mime_type) {
        if let Some(data) = payload["body"]["data"].as_str() {
            return decode_base64_url(data);
        }
    }

    // Search parts
    if let Some(parts) = payload["parts"].as_array() {
        for part in parts {
            let body = extract_body(part, mime_type);
            if !body.is_empty() {
                return body;
            }
        }
    }

    String::new()
}

fn decode_base64_url(s: &str) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD
        .decode(s.replace('-', "+").replace('_', "/"))
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default()
}

fn base64_url_encode(data: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    URL_SAFE_NO_PAD.encode(data)
}

fn build_raw_message(email: &NewEmail) -> String {
    let to = email.to.join(", ");
    format!(
        "To: {}\r\nSubject: {}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{}",
        to, email.subject, email.body
    )
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}
