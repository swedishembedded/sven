// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

/// Thin wrapper over `grep` / ripgrep with sensible codebase defaults:
/// always excludes .git/, target/, node_modules/, dist/, __pycache__/.
pub struct SearchCodebaseTool;

#[async_trait]
impl Tool for SearchCodebaseTool {
    fn name(&self) -> &str {
        "search_codebase"
    }

    fn description(&self) -> &str {
        "Ripgrep across the codebase with standard exclusions: \
         .git/ target/ node_modules/ dist/ __pycache__/ *.lock\n\
         Same regex syntax as grep. Use for broad whole-repo exploration.\n\
         Use grep (not this) when you need output_mode, context_lines, or targeted search.\n\
         Use glob when searching by filename. query: regex. include: glob file filter. \
         case_sensitive: true. limit: 100."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Pattern or text to search for (supports regex)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob filter for file types, e.g. '*.rs' or '*.{ts,tsx}'"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default true)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 100)"
                }
            },
            "required": ["query", "path", "include", "case_sensitive", "limit"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => {
                let args_preview =
                    serde_json::to_string(&call.args).unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "missing required parameter 'query'. Received: {}",
                        args_preview
                    ),
                );
            }
        };
        let path = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let include = call
            .args
            .get("include")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let case_sensitive = call
            .args
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let limit = call
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        debug!(query = %query, path = %path, "search_codebase tool");

        // Build rg command with exclusions
        let has_rg = tokio::process::Command::new("which")
            .arg("rg")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        let output = if has_rg {
            let mut args = vec![
                "--vimgrep".to_string(),
                "--color".to_string(),
                "never".to_string(),
                "--no-heading".to_string(),
                // Exclude build artifacts
                "--glob".to_string(),
                "!.git/**".to_string(),
                "--glob".to_string(),
                "!target/**".to_string(),
                "--glob".to_string(),
                "!node_modules/**".to_string(),
                "--glob".to_string(),
                "!dist/**".to_string(),
                "--glob".to_string(),
                "!__pycache__/**".to_string(),
                "--glob".to_string(),
                "!*.lock".to_string(),
            ];
            if !case_sensitive {
                args.push("--ignore-case".to_string());
            }
            if let Some(glob) = &include {
                args.push("-g".to_string());
                args.push(glob.clone());
            }
            args.push(query.clone());
            args.push(path.clone());

            tokio::process::Command::new("rg")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .output()
                .await
        } else {
            let mut cmd_parts = vec!["grep -rn".to_string()];
            if !case_sensitive {
                cmd_parts.push("-i".to_string());
            }
            cmd_parts.push("--exclude-dir=.git --exclude-dir=target --exclude-dir=node_modules --exclude-dir=dist".to_string());
            if let Some(glob) = &include {
                cmd_parts.push(format!("--include={glob}"));
            }
            cmd_parts.push(shell_escape(&query));
            cmd_parts.push(shell_escape(&path));

            tokio::process::Command::new("sh")
                .arg("-c")
                .arg(cmd_parts.join(" "))
                .stdin(std::process::Stdio::null())
                .output()
                .await
        };

        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = text.lines().take(limit).collect();
                if lines.is_empty() {
                    ToolOutput::ok(&call.id, "(no matches)")
                } else {
                    let total = text.lines().count();
                    let mut result = lines.join("\n");
                    if total > limit {
                        result
                            .push_str(&format!("\n...[{} more matches not shown]", total - limit));
                    }
                    ToolOutput::ok(&call.id, result)
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("search_codebase error: {e}")),
        }
    }
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "s1".into(),
            name: "search_codebase".into(),
            args,
        }
    }

    #[tokio::test]
    async fn finds_in_sven_codebase() {
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "ToolRegistry",
                "path": "/data/agents/sven/crates/sven-tools/src"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(!out.content.contains("(no matches)"));
    }

    #[tokio::test]
    async fn missing_query_is_error() {
        let out = SearchCodebaseTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'query'"));
    }

    #[tokio::test]
    async fn include_glob_narrows_results() {
        // Search only in .toml files — should not return .rs matches
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "version",
                "path": "/data/agents/sven",
                "include_glob": "*.toml"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // All matched lines should come from .toml files
        if !out.content.contains("(no matches)") {
            assert!(
                out.content.contains(".toml"),
                "expected .toml files in results: {}",
                &out.content[..out.content.len().min(300)]
            );
        }
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "TOOLREGISTRY",
                "path": "/data/agents/sven/crates/sven-tools/src",
                "case_sensitive": false
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Should find ToolRegistry in a case-insensitive way
        assert!(
            !out.content.contains("(no matches)"),
            "expected case-insensitive match for TOOLREGISTRY"
        );
    }
}
