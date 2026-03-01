// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

#[derive(Default)]
pub struct WebSearchTool {
    /// Optional API key override (falls back to env BRAVE_API_KEY)
    pub api_key: Option<String>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Real-time web search. Requires BRAVE_API_KEY env var. count: 1-10 (default 5).\n\
         Include the current year in queries for recent info (e.g., 'React docs 2026').\n\
         Knowledge cutoff: early 2025 — use this for anything that may have changed since.\n\
         ALWAYS cite sources after answering:\n\
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
                    "description": "Number of results to return (default 5, max 10)"
                }
            },
            "required": ["query", "count"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
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

        // Resolve API key
        let api_key = self
            .api_key
            .clone()
            .or_else(|| std::env::var("BRAVE_API_KEY").ok());

        let Some(api_key) = api_key else {
            return ToolOutput::err(
                &call.id,
                "No Brave Search API key configured. Set the BRAVE_API_KEY environment variable \
                 or configure tools.web.search.api_key in sven.toml.",
            );
        };

        match brave_search(&query, count, &api_key).await {
            Ok(results) => ToolOutput::ok(&call.id, results),
            Err(e) => ToolOutput::err(&call.id, format!("search error: {e}")),
        }
    }
}

async fn brave_search(query: &str, count: usize, api_key: &str) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("sven-agent/0.1")
        .build()?;

    let url = format!(
        "https://api.search.brave.com/res/v1/web/search?q={}&count={}",
        urlencoding(query),
        count
    );

    let resp = client
        .get(&url)
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

    if results.is_empty() {
        return Ok("(no results)".to_string());
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

    Ok(output.join("\n\n"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;

    #[test]
    fn schema_requires_query() {
        let t = WebSearchTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("query")));
    }

    #[tokio::test]
    async fn returns_error_without_api_key() {
        use crate::tool::ToolCall;
        use serde_json::json;

        // Ensure env var is unset for test
        std::env::remove_var("BRAVE_API_KEY");

        let t = WebSearchTool { api_key: None };
        let call = ToolCall {
            id: "1".into(),
            name: "web_search".into(),
            args: json!({"query": "test"}),
        };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("BRAVE_API_KEY"));
    }
}
