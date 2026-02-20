use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

const DEFAULT_MAX_CHARS: usize = 50_000;

pub struct WebFetchTool;

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str { "web_fetch" }

    fn description(&self) -> &str {
        "Fetch a URL and return its content. HTML pages are converted to plain text. \
         JSON responses are pretty-printed. Output is capped at max_chars characters."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (http or https)"
                },
                "max_chars": {
                    "type": "integer",
                    "description": "Maximum characters to return (default 50000)"
                }
            },
            "required": ["url"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let url = match call.args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'url'"),
        };
        let max_chars = call.args.get("max_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_CHARS as u64) as usize;

        debug!(url = %url, "web_fetch tool");

        match fetch_url(&url, max_chars).await {
            Ok(content) => ToolOutput::ok(&call.id, content),
            Err(e) => ToolOutput::err(&call.id, format!("fetch error: {e}")),
        }
    }
}

async fn fetch_url(url: &str, max_chars: usize) -> anyhow::Result<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("sven-agent/0.1")
        .build()?;

    let response = client.get(url).send().await?;
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();

    let body = response.text().await?;

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

    if content.len() > max_chars {
        Ok(format!(
            "{}...[truncated at {max_chars} chars; total {} chars]",
            &content[..max_chars],
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
        use crate::tool::Tool;
        let t = WebFetchTool;
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("url")));
    }
}
