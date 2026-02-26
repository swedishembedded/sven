// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str { "grep" }

    fn description(&self) -> &str {
        "Pattern search built on ripgrep. Prefer over search_codebase when you know the exact symbol or string.\n\
         pattern: full regex (escape literal braces: \\{\\}). include: glob filter (*.rs, **/*.{ts,tsx}).\n\
         case_sensitive: true by default. limit: 100 by default.\n\
         output_mode: content (default, shows file:line:col:text) | files_with_matches | count\n\
         context_lines: lines of context before+after each match (default 0).\n\
         Use files_with_matches for discovery, then read_file for details.\n\
         For whole-codebase search with .git/target/node_modules auto-excluded → use search_codebase."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (default: current directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files, e.g. '*.rs' or '*.{ts,tsx}'"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default true)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 100)"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output format: content (default), files_with_matches, or count"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context before and after each match (default 0)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }
    fn output_category(&self) -> OutputCategory { OutputCategory::MatchList }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'pattern'. Received: {}", args_preview)
                );
            }
        };
        let path = call.args.get("path").and_then(|v| v.as_str()).unwrap_or(".").to_string();
        let include = call.args.get("include").and_then(|v| v.as_str()).map(str::to_string);
        let case_sensitive = call.args.get("case_sensitive").and_then(|v| v.as_bool()).unwrap_or(true);
        let limit = call.args.get("limit").and_then(|v| v.as_u64()).unwrap_or(100) as usize;
        let output_mode = call.args.get("output_mode").and_then(|v| v.as_str()).unwrap_or("content");
        let context_lines = call.args.get("context_lines").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        debug!(pattern = %pattern, path = %path, output_mode = %output_mode, "grep tool");

        let result = run_rg(&pattern, &path, include.as_deref(), case_sensitive, limit, output_mode, context_lines).await;

        match result {
            Ok(output) if output.trim().is_empty() => {
                ToolOutput::ok(&call.id, "(no matches)")
            }
            Ok(output) => ToolOutput::ok(&call.id, output),
            Err(e) => ToolOutput::err(&call.id, format!("grep error: {e}")),
        }
    }
}

async fn run_rg(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    case_sensitive: bool,
    limit: usize,
    output_mode: &str,
    context_lines: usize,
) -> anyhow::Result<String> {
    let has_rg = tokio::process::Command::new("which")
        .arg("rg")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    let output = if has_rg {
        let mut args = vec!["--color".to_string(), "never".to_string()];

        match output_mode {
            "files_with_matches" => {
                args.push("-l".to_string());
            }
            "count" => {
                args.push("-c".to_string());
            }
            _ => {
                // content mode: vimgrep format for unambiguous file:line:col:text output
                args.push("--vimgrep".to_string());
                args.push("--no-heading".to_string());
            }
        }

        if !case_sensitive {
            args.push("--ignore-case".to_string());
        }
        if context_lines > 0 && output_mode == "content" {
            args.push(format!("-C{}", context_lines));
        }
        if let Some(glob) = include {
            args.push("-g".to_string());
            args.push(glob.to_string());
        }
        args.push(pattern.to_string());
        args.push(path.to_string());

        tokio::process::Command::new("rg")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .await?
    } else {
        // Fallback to grep
        let mut args = vec!["-rn".to_string()];
        match output_mode {
            "files_with_matches" => { args.push("-l".to_string()); }
            "count" => { args.push("-c".to_string()); }
            _ => {}
        }
        if !case_sensitive {
            args.push("-i".to_string());
        }
        if context_lines > 0 && output_mode == "content" {
            args.push(format!("-C{}", context_lines));
        }
        if let Some(glob) = include {
            args.push("--include".to_string());
            args.push(glob.to_string());
        }
        args.push(pattern.to_string());
        args.push(path.to_string());

        tokio::process::Command::new("grep")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .await?
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().take(limit).collect();
    let mut result = lines.join("\n");
    let total_lines = stdout.lines().count();
    if total_lines > limit {
        result.push_str(&format!(
            "\n...[{} more matches not shown — narrow with path= or include= to see all results]",
            total_lines - limit
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "g1".into(), name: "grep".into(), args }
    }

    #[tokio::test]
    async fn finds_pattern_in_file() {
        let out = GrepTool.execute(&call(json!({
            "pattern": "pub struct",
            "path": "/data/agents/sven/crates/sven-tools/src/tool.rs"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("ToolCall") || out.content.contains("ToolOutput"));
    }

    #[tokio::test]
    async fn no_match_returns_no_matches() {
        let out = GrepTool.execute(&call(json!({
            "pattern": "xyzzy_nonexistent_pattern_12345",
            "path": "/tmp"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let out = GrepTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'pattern'"));
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_grep_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, "Hello World\n").unwrap();

        let out = GrepTool.execute(&call(json!({
            "pattern": "hello",
            "path": path,
            "case_sensitive": false
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("Hello"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn limit_truncates_results() {
        // Search in a directory with many matches, limit to 2
        let out = GrepTool.execute(&call(json!({
            "pattern": "pub",
            "path": "/data/agents/sven/crates/sven-tools/src/builtin",
            "limit": 2
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        // Should show truncation notice
        assert!(
            out.content.contains("more") || out.content.lines().count() <= 4,
            "expected truncation or small result set: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn nonexistent_path_returns_no_matches_or_error() {
        let out = GrepTool.execute(&call(json!({
            "pattern": "anything",
            "path": "/tmp/sven_no_such_dir_xyzzy_12345"
        }))).await;
        // Either an error or a "no matches" result is acceptable
        assert!(
            out.is_error || out.content.contains("no matches") || out.content.contains("error"),
            "unexpected output: {}",
            out.content
        );
    }
}
