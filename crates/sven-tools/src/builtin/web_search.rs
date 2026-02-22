// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
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
    fn name(&self) -> &str { "web_search" }

    fn description(&self) -> &str {
        "Search the web for real-time information about any topic.\n\n\
         ## Usage\n\
         - Returns results with title, URL, and description\n\
         - Search query can be specific or broad\n\
         - Results are ranked by relevance\n\
         - Returns up to 10 results (default 5)\n\n\
         ## CRITICAL - Current Year Handling\n\
         - Today's year is 2026 (update this dynamically when needed)\n\
         - When searching for recent docs, include year: 'React 2026 documentation'\n\
         - Do NOT use outdated years in queries\n\
         - Examples: 'Python 3.13 2026', 'TypeScript 5.0 latest'\n\n\
         ## When to Use\n\
         - Current events and recent technology changes\n\
         - Library/framework documentation (APIs change frequently)\n\
         - Verifying facts beyond training data (knowledge cutoff: Feb 2025)\n\
         - Real-time information needs\n\n\
         ## When NOT to Use\n\
         - Historical information (Wikipedia is better)\n\
         - Deep technical questions (Stack Overflow via grep of local docs)\n\
         - Searching codebases (use grep tool instead)\n\n\
         ## Citation Requirements (MANDATORY)\n\
         After answering the user's question, you MUST include a Sources section:\n\
         Sources:\n\
         - [Source Title](URL)\n\
         - [Source Title](URL)\n\
         This applies to ANY answer using web_search results.\n\n\
         ## Examples\n\
         <example>\n\
         Search for recent library updates:\n\
         web_search: query=\"React 2026 latest features and changes\"\n\
         </example>\n\
         <example>\n\
         Search for current news:\n\
         web_search: query=\"AI coding assistants 2026 latest\"\n\
         </example>\n\
         <example>\n\
         Search for current documentation:\n\
         web_search: query=\"Node.js 22 API documentation 2026\"\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Include year in query for recent information\n\
         - Always cite sources in response\n\
         - Requires BRAVE_API_KEY environment variable\n\
         - Maximum 10 results per search (use count parameter)\n\
         - Filter results to most relevant ones for user"
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
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'query'"),
        };
        let count = call.args.get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(10) as usize;

        debug!(query = %query, count, "web_search tool");

        // Resolve API key
        let api_key = self.api_key.clone()
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
        let title = r.get("title").and_then(|v| v.as_str()).unwrap_or("(no title)");
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
        let call = ToolCall { id: "1".into(), name: "web_search".into(), args: json!({"query": "test"}) };
        let out = t.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("BRAVE_API_KEY"));
    }
}
