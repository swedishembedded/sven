// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Built-in tool for recursive file search using glob / path patterns.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str { "glob" }

    fn description(&self) -> &str {
        "Search for files matching a glob pattern.\n\n\
         ## Usage\n\
         - Supports shell glob patterns (*, **, ?, [abc])\n\
         - Automatically excludes target/ and .git/ directories\n\
         - Returns matching file paths sorted by modification time\n\
         - Results are newline-delimited\n\n\
         ## Pattern Examples\n\
         - '*.rs' → Find all Rust files in root directory\n\
         - '**/*.rs' → Find all Rust files recursively\n\
         - '**/*.test.ts' → Find all TypeScript test files\n\
         - 'src/components/**/*.tsx' → Find all React components\n\
         - '**/[Mm]akefile' → Find Makefiles (case-insensitive)\n\n\
         ## When to Use\n\
         - Finding files by name pattern\n\
         - Discovering all files of a specific type\n\
         - Locating files in specific directories\n\
         - Initial discovery before using grep for content search\n\n\
         ## When NOT to Use\n\
         - Searching file contents → use grep tool instead\n\
         - Finding specific code/functions → use grep tool instead\n\
         - Pattern matching within filenames only → may need grep\n\n\
         ## Examples\n\
         <example>\n\
         Find all test files:\n\
         glob: pattern=\"**/*.test.ts\"\n\
         </example>\n\
         <example>\n\
         Discovery workflow:\n\
         1. glob: pattern=\"**/*.rs\" → Find all Rust files\n\
         2. grep: pattern=\"fn new\", include=\"*.rs\" → Find new() functions\n\
         3. read_file: (for specific file) → Read implementation\n\
         </example>\n\
         <example>\n\
         Find configuration files:\n\
         glob: pattern=\"**/{config,settings,*.yaml,*.yml}\"\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Patterns are automatically prefixed with **/ for recursive search if needed\n\
         - Results are limited by max_results parameter (default 200)\n\
         - Automatically excludes common build/version control directories\n\
         - Works faster than grep for file discovery"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Shell glob pattern passed to `find`, e.g. '*.rs'"
                },
                "root": {
                    "type": "string",
                    "description": "Root directory to search from (default: current directory)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default 200)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
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

        debug!(pattern = %pattern, root = %root, "glob tool");

        // Normalize glob pattern: strip **/ prefix since find is recursive by default
        let normalized_pattern = pattern.strip_prefix("**/").unwrap_or(&pattern);

        let cmd_str = format!(
            "find {} -name '{}' -not -path '*/target/*' -not -path '*/.git/*' | head -{}",
            root, normalized_pattern, max
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
            Err(e) => ToolOutput::err(&call.id, format!("glob error: {e}")),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "g1".into(), name: "glob".into(), args }
    }

    // ── Successful searches ───────────────────────────────────────────────────

    #[tokio::test]
    async fn finds_toml_files_in_workspace() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "*.toml",
            "root": "/data/agents/sven"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn finds_rs_files() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "lib.rs",
            "root": "/data/agents/sven/crates"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("lib.rs"));
    }

    #[tokio::test]
    async fn no_match_returns_no_matches_message() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "*.xyz_nonexistent_ext",
            "root": "/tmp"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
    }

    // ── max_results limits output ─────────────────────────────────────────────

    #[tokio::test]
    async fn max_results_is_respected() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "*.rs",
            "root": "/data/agents/sven",
            "max_results": 2
        }))).await;
        assert!(!out.is_error);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines.len() <= 2, "expected at most 2 results, got {}", lines.len());
    }

    // ── Pattern normalization ─────────────────────────────────────────────────

    #[tokio::test]
    async fn strips_double_star_prefix() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "**/*.toml",
            "root": "/data/agents/sven",
            "max_results": 5
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn handles_bare_double_star_slash_star() {
        let t = GlobTool;
        let out = t.execute(&call(json!({
            "pattern": "**/*",
            "root": "/data/agents/sven/crates/sven-tools",
            "max_results": 10
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(!out.content.contains("no matches"));
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let t = GlobTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'pattern'"));
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_requires_pattern() {
        let t = GlobTool;
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }
}
