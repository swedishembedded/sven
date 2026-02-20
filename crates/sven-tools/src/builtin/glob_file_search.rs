use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct GlobFileSearchTool;

#[async_trait]
impl Tool for GlobFileSearchTool {
    fn name(&self) -> &str { "glob_file_search" }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern. \
         Returns a newline-delimited list of matching paths. \
         Automatically excludes .git/ and target/ directories."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Shell glob pattern, e.g. '*.rs' or '**/*.toml'"
                },
                "root": {
                    "type": "string",
                    "description": "Root directory to search from (default: current directory)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 200)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'pattern'"),
        };
        let root = call.args.get("root").and_then(|v| v.as_str()).unwrap_or(".").to_string();
        let max = call.args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(200) as usize;

        debug!(pattern = %pattern, root = %root, "glob_file_search tool");

        let cmd_str = format!(
            "find {} -name '{}' -not -path '*/target/*' -not -path '*/.git/*' -not -path '*/node_modules/*' | head -{}",
            root, pattern, max
        );

        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd_str)
            .output()
            .await;

        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout).to_string();
                if text.trim().is_empty() {
                    ToolOutput::ok(&call.id, "(no matches)")
                } else {
                    ToolOutput::ok(&call.id, text.trim_end().to_string())
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("glob_file_search error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "g1".into(), name: "glob_file_search".into(), args }
    }

    #[tokio::test]
    async fn finds_toml_files() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.toml",
            "root": "/data/agents/sven"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn no_match_returns_no_matches_message() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.xyz_nonexistent_ext",
            "root": "/tmp"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
    }

    #[tokio::test]
    async fn max_results_is_respected() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({
            "pattern": "*.rs",
            "root": "/data/agents/sven",
            "max_results": 2
        }))).await;
        assert!(!out.is_error);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines.len() <= 2);
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let t = GlobFileSearchTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'pattern'"));
    }
}
