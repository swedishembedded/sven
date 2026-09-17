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

/// Google's OAuth2 token endpoint.
const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

/// How long before nominal expiry a token is already treated as expired, so a
/// request cannot go out holding a token that dies in flight.
const EXPIRY_SKEW_SECS: i64 = 60;

/// Gmail API token storage.
#[derive(Debug, Serialize, Deserialize)]
struct GmailToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: Option<i64>,
}

impl GmailToken {
    /// Whether this token must be refreshed before use at `now` (unix seconds).
    ///
    /// A token with no recorded expiry is taken at face value: the setup flow
    /// stored what Google returned, and Gmail rejects it if it is stale.
    fn is_expired_at(&self, now: i64) -> bool {
        self.expires_at
            .is_some_and(|expires_at| now >= expires_at - EXPIRY_SKEW_SECS)
    }
}

/// The subset of Google's token response this provider consumes.
#[derive(Debug, Deserialize)]
struct RefreshResponse {
    access_token: String,
    expires_in: Option<i64>,
    refresh_token: Option<String>,
}

/// Gmail REST API email provider.
pub struct GmailProvider {
    client_id: String,
    client_secret: String,
    token_path: PathBuf,
    /// OAuth2 token endpoint. Overridable so tests can drive the refresh
    /// against a local server instead of Google.
    token_endpoint: String,
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
            token_endpoint: GOOGLE_TOKEN_ENDPOINT.to_string(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("Gmail HTTP client"),
        }
    }

    /// A usable access token, refreshed and re-persisted if the stored one has
    /// expired.
    async fn access_token(&self) -> anyhow::Result<String> {
        let token = self.read_token().await?;
        if !token.is_expired_at(chrono::Utc::now().timestamp()) {
            return Ok(token.access_token);
        }

        let refresh_token = token.refresh_token.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Gmail access token expired and the stored token at {} carries \
                 no refresh_token. Re-run the OAuth2 setup flow, requesting \
                 offline access so Google issues one.",
                self.token_path.display()
            )
        })?;

        debug!("Gmail access token expired; refreshing");
        let refreshed = self.fetch_refreshed(&refresh_token).await?;
        self.write_token(&refreshed).await?;
        Ok(refreshed.access_token)
    }

    async fn read_token(&self) -> anyhow::Result<GmailToken> {
        let text = tokio::fs::read_to_string(&self.token_path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Gmail token file not found at {}. \
                 Run the OAuth2 setup flow first: {e}",
                    self.token_path.display()
                )
            })?;
        Ok(serde_json::from_str(&text)?)
    }

    /// Exchanges `refresh_token` for a fresh access token.
    async fn fetch_refreshed(&self, refresh_token: &str) -> anyhow::Result<GmailToken> {
        let response = self
            .client
            .post(&self.token_endpoint)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            // A revoked or expired refresh token needs a different action from
            // the user than a transient failure, so say both.
            anyhow::bail!(
                "Gmail token refresh failed ({status}): {body}. If the refresh \
                 token was revoked or has expired, re-run the OAuth2 setup flow."
            );
        }

        let parsed: RefreshResponse = serde_json::from_str(&body)?;
        Ok(GmailToken {
            access_token: parsed.access_token,
            // Google omits refresh_token on an ordinary refresh; keeping the
            // existing one is what makes the next refresh possible.
            refresh_token: parsed
                .refresh_token
                .or_else(|| Some(refresh_token.to_string())),
            expires_at: parsed
                .expires_in
                .map(|secs| chrono::Utc::now().timestamp() + secs),
        })
    }

    /// Persists `token`, replacing the file atomically and keeping it readable
    /// only by its owner — it holds a long-lived Google credential.
    async fn write_token(&self, token: &GmailToken) -> anyhow::Result<()> {
        let json = serde_json::to_vec_pretty(token)?;
        let dir = self
            .token_path
            .parent()
            .unwrap_or(std::path::Path::new("."));
        let tmp = self.token_path.with_extension("tmp");

        tokio::fs::create_dir_all(dir).await?;
        tokio::fs::write(&tmp, &json).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).await?;
        }
        tokio::fs::rename(&tmp, &self.token_path).await?;
        Ok(())
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

/// Percent-encodes `s` for use in a URL query, escaping everything outside the
/// RFC 3986 unreserved set.
///
/// Encoding operates on UTF-8 **bytes**. Testing `char::is_alphanumeric`
/// instead let every non-ASCII letter through unescaped (it is Unicode-aware),
/// and `%{code_point:02X}` produced a Latin-1 escape below U+0100 and an
/// invalid four-digit one above it.
fn urlencoding(s: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_at: Option<i64>, refresh_token: Option<&str>) -> GmailToken {
        GmailToken {
            access_token: "access".into(),
            refresh_token: refresh_token.map(str::to_string),
            expires_at,
        }
    }

    #[test]
    fn token_without_a_recorded_expiry_is_used_as_is() {
        assert!(!token(None, None).is_expired_at(1_000_000));
    }

    #[test]
    fn token_is_expired_once_it_is_within_the_skew_of_expiry() {
        let expiry = 1_000_000;
        let t = token(Some(expiry), None);
        assert!(
            !t.is_expired_at(expiry - EXPIRY_SKEW_SECS - 1),
            "comfortably before expiry the token is still good"
        );
        assert!(
            t.is_expired_at(expiry - EXPIRY_SKEW_SECS),
            "inside the skew it must refresh, so no request goes out with a \
             token that dies in flight"
        );
        assert!(t.is_expired_at(expiry + 1), "past expiry it must refresh");
    }

    /// The failure a user actually hits when the setup flow did not request
    /// offline access: it must name the fix, not surface a bare HTTP 401.
    #[tokio::test]
    async fn expired_token_without_a_refresh_token_explains_the_fix() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("gmail-token.json");
        std::fs::write(&path, serde_json::to_string(&token(Some(0), None)).unwrap()).unwrap();

        let provider = GmailProvider::new("id", "secret", &path);
        let err = provider.access_token().await.unwrap_err().to_string();
        assert!(
            err.contains("refresh_token") && err.contains("OAuth2 setup flow"),
            "error must tell the user what to do, got: {err}"
        );
    }

    /// The token file holds a long-lived Google credential, so rewriting it
    /// must not widen its permissions.
    #[tokio::test]
    async fn refreshed_token_is_persisted_owner_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("gmail-token.json");
        let provider = GmailProvider::new("id", "secret", &path);

        provider
            .write_token(&token(Some(123), Some("refresh")))
            .await
            .unwrap();

        let stored: GmailToken =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(stored.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(stored.expires_at, Some(123));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group/other must have no access");
        }
    }

    /// Gmail search queries carry user- and agent-supplied text. Percent
    /// encoding operates on UTF-8 bytes; encoding `char` code points instead
    /// produces a wrong escape for anything outside ASCII and an entirely
    /// invalid one above U+00FF.
    #[test]
    fn urlencoding_escapes_utf8_bytes() {
        assert_eq!(urlencoding("a b"), "a%20b");
        assert_eq!(urlencoding("from:a@b.com"), "from%3Aa%40b.com");
        assert_eq!(urlencoding("aao"), "aao");
        assert_eq!(urlencoding("\u{e4}"), "%C3%A4", "U+00E4 is two UTF-8 bytes");
        assert_eq!(urlencoding("\u{20ac}"), "%E2%82%AC", "U+20AC is three");
    }
}
