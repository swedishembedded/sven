// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};

use crate::provenance::{attach_web_provenance, now_unix};
use crate::public_net;

/// Default character ceiling for fetched page content.
/// 20 K chars ≈ 5,000 tokens - fits comfortably within a 40 K-token context window.
pub(crate) const DEFAULT_MAX_CHARS: usize = 20_000;

pub struct WebFetchTool {
    /// Cap applied when the caller does not pass `max_chars`, from
    /// `tools.web.fetch_max_chars`.
    default_max_chars: usize,
    /// Only the public internet is reachable (`tools.web.public_only`; see
    /// [`crate::public_net`]).
    public_only: bool,
}

impl WebFetchTool {
    /// A tool whose default cap comes from config.
    #[must_use]
    pub fn new(default_max_chars: usize) -> Self {
        Self {
            default_max_chars,
            public_only: false,
        }
    }

    /// The same tool, reaching only the public internet.
    #[must_use]
    pub fn public_only(mut self) -> Self {
        self.public_only = true;
        self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CHARS)
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn summary(&self) -> &str {
        "Fetch a web page by URL and return it as readable text."
    }

    fn description(&self) -> &str {
        "Fetch an http(s) web page and return it as readable text (HTML becomes markdown).\n\
         Read-only; no authentication."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "http or https URL"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Truncate to this many characters (default 20000, tools.web.fetch_max_chars)"
                }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let url = match call.args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'url'"),
        };
        let max_chars = output_chars(
            call.args.get("max_chars").and_then(|v| v.as_u64()),
            self.default_max_chars,
        );

        debug!(url = %url, "web_fetch tool");

        match fetch_url(&url, max_chars, self.public_only).await {
            // Provenance names exactly what was fetched: this tool never
            // writes memory itself - it only attaches the provenance of what
            // it returned.
            Ok(content) => {
                attach_web_provenance(ToolOutput::ok(&call.id, content), &url, now_unix())
            }
            Err(e) => ToolOutput::err(&call.id, format!("fetch error: {e}")),
        }
    }
}

/// Redirects followed before giving up.
const MAX_REDIRECTS: usize = 3;

/// The most of a response body read, after decompression: the rest is never
/// downloaded. Many hosted agents share one memory budget, so one page (or a
/// small body that decompresses to gigabytes) must not take it.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The most characters returned, whatever `max_chars` the model asks for.
const MAX_OUTPUT_CHARS: usize = 100_000;

/// The characters to return: what the model asked for, else the configured
/// default, never more than [`MAX_OUTPUT_CHARS`].
fn output_chars(requested: Option<u64>, default: usize) -> usize {
    requested
        .map_or(default, |n| usize::try_from(n).unwrap_or(usize::MAX))
        .min(MAX_OUTPUT_CHARS)
}

/// The body of `response`, read up to [`MAX_BODY_BYTES`] as it arrives, and
/// whether it went on past that.
async fn capped_body(mut response: reqwest::Response) -> anyhow::Result<(String, bool)> {
    let mut body = Vec::new();
    let mut cut = false;
    while let Some(chunk) = response.chunk().await? {
        let room = MAX_BODY_BYTES - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            cut = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok((String::from_utf8_lossy(&body).into_owned(), cut))
}

async fn fetch_url(url: &str, max_chars: usize, public_only: bool) -> anyhow::Result<String> {
    let max_chars = max_chars.min(MAX_OUTPUT_CHARS);
    let builder = sven_http::client_builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("sven-agent/0.1");
    let client = if public_only {
        builder
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(public_net::PublicOnly))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() > MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                match public_net::refuse_literal(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(why) => attempt.error(why),
                }
            }))
    } else {
        builder.redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
    }
    .build()?;

    let parsed = reqwest::Url::parse(url)?;
    if public_only {
        public_net::refuse_literal(&parsed).map_err(|why| anyhow::anyhow!(why))?;
    }
    let response = client.get(parsed).send().await?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let (body, body_cut) = capped_body(response).await?;

    let content = if content_type.contains("html") {
        html_to_text(&body)
    } else if content_type.contains("json") {
        match serde_json::from_str::<Value>(&body) {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(body),
            Err(_) => body,
        }
    } else {
        body
    };

    if body_cut && content.len() <= max_chars {
        return Ok(format!(
            "{content}...[truncated: the page is larger than {MAX_BODY_BYTES} bytes]"
        ));
    }
    if content.len() > max_chars {
        // The model picks both the URL and `max_chars`, so this offset is
        // attacker- and model-influenced and lands mid-character on any
        // non-ASCII page. Round down to the nearest boundary.
        let cut = content.floor_char_boundary(max_chars);
        Ok(format!(
            "{}...[truncated at {max_chars} chars; total {} chars]",
            &content[..cut],
            content.len()
        ))
    } else {
        Ok(content)
    }
}

/// Convert HTML to plain text using html2text.
fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 100)
}

impl ToolDisplay for WebFetchTool {
    fn display_name(&self) -> &str {
        "WebFetch"
    }
    fn icon(&self) -> &str {
        "🔗"
    }
    fn category(&self) -> &str {
        "web"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        sven_tool_api::tool_summary::tool_smart_summary("web_fetch", args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_tags() {
        let html = "<html><body><h1>Hello</h1><p>World</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("<h1>"));
    }

    #[test]
    fn schema_requires_url() {
        use sven_tool_api::tool::Tool;
        let t = WebFetchTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }

    /// A failed fetch carries no provenance claim - there is nothing this
    /// tool can honestly say it fetched. Uses a malformed URL so the failure
    /// happens at request construction, before any real network I/O.
    #[tokio::test]
    async fn a_failed_fetch_attaches_no_provenance() {
        use sven_tool_api::tool::ToolCall;

        let t = WebFetchTool::default();
        let call = ToolCall {
            id: "call-1".into(),
            name: "web_fetch".into(),
            args: json!({"url": "not a url", "max_chars": 100}),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error, "a malformed URL must fail: {}", out.content);
        assert!(out.provenance.is_none());
    }

    /// A page that never ends (or a body decompressing to gigabytes) is read
    /// only up to [`MAX_BODY_BYTES`]: the hosted runner holds many agents in
    /// one memory budget, and one page must not take it.
    #[tokio::test]
    async fn an_endless_page_is_read_only_up_to_the_cap() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n",
                )
                .await;
            let chunk = vec![b'a'; 64 * 1024];
            // Far past the cap; the reader must stop and hang up first.
            for _ in 0..(4 * MAX_BODY_BYTES / chunk.len()) {
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
            }
        });
        let text = fetch_url(&format!("http://{address}/"), usize::MAX, false)
            .await
            .unwrap();
        assert!(
            text.contains("truncated"),
            "{}",
            &text[text.len().saturating_sub(200)..]
        );
        assert!(text.len() <= MAX_OUTPUT_CHARS + 200, "{}", text.len());
    }

    /// The model chooses `max_chars`; an absurd value is clamped.
    #[test]
    fn a_model_cannot_raise_the_output_past_the_ceiling() {
        assert_eq!(output_chars(Some(u64::MAX), 20_000), MAX_OUTPUT_CHARS);
        assert_eq!(output_chars(Some(500), 20_000), 500);
        assert_eq!(output_chars(None, 20_000), 20_000);
    }
}
