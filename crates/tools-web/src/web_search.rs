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

use crate::config::SvenEndpoint;

/// Where a search goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchBackend {
    /// A sven service's `/search`, authenticated with its key; it searches
    /// and bills the account the key belongs to.
    Sven(SvenEndpoint),
    /// Brave Search, with this key or `BRAVE_API_KEY`.
    Brave { api_key: Option<String> },
    /// No search is possible, for this reason.
    Unavailable(String),
}

impl Default for SearchBackend {
    fn default() -> Self {
        Self::Brave { api_key: None }
    }
}

#[derive(Default)]
pub struct WebSearchTool {
    pub backend: SearchBackend,
}

impl WebSearchTool {
    /// Searching Brave with `api_key`, or `BRAVE_API_KEY` without one.
    #[must_use]
    pub fn brave(api_key: Option<String>) -> Self {
        Self {
            backend: SearchBackend::Brave { api_key },
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn summary(&self) -> &str {
        "Search the web and return result titles, links and snippets."
    }

    fn description(&self) -> &str {
        "Web search. Put the current year in queries\n\
         for recent topics. Cite the sources you used after answering:\n\
         Sources:\n\
         - [Title](URL)"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Search query"
                },
                "count": {
                    "type": "integer",
                    "description": "Results, 1-10 (default 5)"
                }
            },
            "required": ["query"],
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
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'query'"),
        };
        let count = call
            .args
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        debug!(query = %query, count, "web_search tool");

        match &self.backend {
            SearchBackend::Unavailable(reason) => ToolOutput::err(&call.id, reason.clone()),
            SearchBackend::Sven(endpoint) => {
                let url = sven_search_url(&endpoint.base_url);
                match sven_search(&url, &query, count, &endpoint.api_key).await {
                    Ok(results) => attach_web_provenance(
                        ToolOutput::ok(&call.id, results),
                        &format!("{url}?q={}", urlencoding(&query)),
                        now_unix(),
                    ),
                    Err(e) => ToolOutput::err(&call.id, format!("search error: {e}")),
                }
            }
            SearchBackend::Brave { api_key } => {
                let api_key = api_key
                    .clone()
                    .or_else(|| std::env::var("BRAVE_API_KEY").ok());
                let Some(api_key) = api_key else {
                    return ToolOutput::err(
                        &call.id,
                        "No search service is configured. Sign in to a sven service (`sven rc`), \
                         configure a `providers.sven` entry with a key, or set BRAVE_API_KEY or \
                         tools.web.search.api_key for Brave.",
                    );
                };
                let url = search_url(&query, count);
                match brave_search(&url, count, &api_key).await {
                    // Provenance names the actual API URL queried - this tool never
                    // writes memory itself, it only attaches the provenance of what
                    // it returned.
                    Ok(results) => {
                        attach_web_provenance(ToolOutput::ok(&call.id, results), &url, now_unix())
                    }
                    Err(e) => ToolOutput::err(&call.id, format!("search error: {e}")),
                }
            }
        }
    }
}

/// A sven service's search route under its API base (`https://host/v1`).
fn sven_search_url(base_url: &str) -> String {
    format!("{}/search", base_url.trim_end_matches('/'))
}

/// Searches through a sven service: `POST {query, count}` with its key,
/// answered `{results: [{title, url, description}]}`. A refusal (no credit,
/// a revoked key) carries the service's own message, which the agent shows.
async fn sven_search(
    url: &str,
    query: &str,
    count: usize,
    api_key: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("sven-agent/0.1")
        .build()?;
    let resp = client
        .post(url)
        .bearer_auth(api_key)
        .json(&json!({ "query": query, "count": count }))
        .send()
        .await?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let message = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map_or_else(
                || format!("the search service returned status {status}"),
                str::to_string,
            );
        anyhow::bail!(message);
    }
    let results = body
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    Ok(format_results(results, count))
}

/// The Brave Search API URL a query actually resolves to - the "url" this
/// tool's provenance names. A pure function so it can be pinned by a test
/// without a network call.
fn search_url(query: &str, count: usize) -> String {
    format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding(query),
        count
    )
}

async fn brave_search(url: &str, count: usize, api_key: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("sven-agent/0.1")
        .build()?;

    let resp = client
        .get(url)
        .header("Accept", "application/json")
        .header("Accept-Encoding", "gzip")
        .header("X-Subscription-Token", api_key)
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("Brave API returned status {}", resp.status());
    }

    let json: Value = resp.json().await?;

    let results = json
        .get("web")
        .and_then(|w| w.get("results"))
        .and_then(|r| r.as_array())
        .map(|arr| arr.as_slice())
        .unwrap_or(&[]);
    Ok(format_results(results, count))
}

/// Results as the agent reads them: numbered title, link and snippet.
fn format_results(results: &[Value], count: usize) -> String {
    if results.is_empty() {
        return "(no results)".to_string();
    }

    let mut output = Vec::new();
    for (i, r) in results.iter().enumerate().take(count) {
        let title = r
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(no title)");
        let url = r.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let desc = r.get("description").and_then(|v| v.as_str()).unwrap_or("");
        output.push(format!("{}. **{}**\n   {}\n   {}", i + 1, title, url, desc));
    }

    output.join("\n\n")
}

fn urlencoding(s: &str) -> String {
    let mut encoded = String::new();
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => encoded.push(c),
            ' ' => encoded.push('+'),
            c => {
                for byte in c.to_string().as_bytes() {
                    encoded.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    encoded
}

impl ToolDisplay for WebSearchTool {
    fn display_name(&self) -> &str {
        "WebSearch"
    }
    fn icon(&self) -> &str {
        "🌐"
    }
    fn category(&self) -> &str {
        "web"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        sven_tool_api::tool_summary::tool_smart_summary("web_search", args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_tool_api::tool::Tool;

    #[test]
    fn schema_requires_query() {
        let t = WebSearchTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("query")));
    }

    #[tokio::test]
    async fn returns_error_without_api_key() {
        use serde_json::json;
        use sven_tool_api::tool::ToolCall;

        // Ensure env var is unset for test
        std::env::remove_var("BRAVE_API_KEY");

        let t = WebSearchTool::brave(None);
        let call = ToolCall {
            id: "1".into(),
            name: "web_search".into(),
            args: json!({"query": "test"}),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("BRAVE_API_KEY"));
        assert!(out.content.contains("sven rc"));
        assert!(
            out.provenance.is_none(),
            "no API key means nothing was actually fetched"
        );
    }

    /// A one-request HTTP server: answers `status` with `body`, and hands
    /// back the request it received (head and body, as text).
    async fn one_answer(
        status: u16,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request);
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text[..end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if request.len() >= end + 4 + length || n == 0 {
                        break;
                    }
                }
            }
            let answer = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(answer.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (base, served)
    }

    fn sven_tool(base_url: String) -> WebSearchTool {
        WebSearchTool {
            backend: SearchBackend::Sven(SvenEndpoint {
                base_url,
                api_key: "sk-sven-account".into(),
            }),
        }
    }

    fn call(args: serde_json::Value) -> sven_tool_api::tool::ToolCall {
        sven_tool_api::tool::ToolCall {
            id: "1".into(),
            name: "web_search".into(),
            args,
        }
    }

    #[tokio::test]
    async fn through_a_sven_service_it_posts_the_query_with_the_key_and_lists_the_results() {
        let (base, served) = one_answer(
            200,
            r#"{"results":[{"title":"Swedish Embedded","url":"https://swedishembedded.com/","description":"Firmware"}]}"#,
        )
        .await;
        let out = sven_tool(base)
            .execute(&call(
                serde_json::json!({"query": "embedded rust", "count": 3}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1. **Swedish Embedded**"));
        assert!(out.content.contains("https://swedishembedded.com/"));
        assert!(out.provenance.is_some());
        let request = served.await.unwrap();
        assert!(request.starts_with("POST /v1/search "), "{request}");
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer sk-sven-account"));
        assert!(request.contains(r#""query":"embedded rust""#) && request.contains(r#""count":3"#));
    }

    #[tokio::test]
    async fn a_refusal_from_the_sven_service_reaches_the_agent_in_its_own_words() {
        let (base, served) = one_answer(
            402,
            r#"{"error":{"message":"the account does not have enough credit for this request","type":"insufficient_quota"}}"#,
        )
        .await;
        let out = sven_tool(base)
            .execute(&call(serde_json::json!({"query": "q"})))
            .await;
        served.await.unwrap();
        assert!(out.is_error);
        assert!(
            out.content.contains("does not have enough credit"),
            "{}",
            out.content
        );
        assert!(out.provenance.is_none());
    }

    #[tokio::test]
    async fn an_unavailable_backend_says_why_without_searching() {
        let tool = WebSearchTool {
            backend: SearchBackend::Unavailable("no sven service is known".into()),
        };
        let out = tool.execute(&call(serde_json::json!({"query": "q"}))).await;
        assert!(out.is_error && out.content.contains("no sven service is known"));
    }

    /// The URL this tool's provenance names must be the exact API URL a
    /// search would query - not a placeholder, and not the raw query text.
    #[test]
    fn search_url_names_the_real_brave_api_endpoint_and_query() {
        let url = search_url("rust async traits", 3);
        assert!(url.starts_with("https://api.search.brave.com/res/v1/web/search?q="));
        assert!(url.contains("rust+async+traits"));
        assert!(url.ends_with("&count=3"));
    }
}
