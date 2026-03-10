// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::store::ContextStore;

pub struct ContextGrepTool {
    pub(crate) store: Arc<Mutex<ContextStore>>,
}

impl ContextGrepTool {
    pub fn new(store: Arc<Mutex<ContextStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ContextGrepTool {
    fn name(&self) -> &str {
        "context_grep"
    }

    fn description(&self) -> &str {
        "Search a memory-mapped context handle with regex. Returns matching lines with positions. \
         This is a cheap pre-filter — use it to narrow down before reading or dispatching \
         sub-agent queries. The full content is never loaded into your context window; only \
         matching lines (up to the limit) are returned.\n\n\
         Common workflow:\n\
         1. context_grep to find where relevant content lives (line numbers + brief context)\n\
         2. context_read on specific matched ranges to understand them in detail\n\
         3. context_query on matched ranges for semantic analysis across many sections\n\n\
         Supports full Rust regex syntax. Use (?i) prefix for case-insensitive matching."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Context handle ID returned by context_open"
                },
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for (Rust regex syntax). \
                                    Use (?i) for case-insensitive."
                },
                "file": {
                    "type": "string",
                    "description": "For directory handles: path substring to limit search to one file"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context to show before and after each match (default 2)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 50)"
                }
            },
            "required": ["handle", "pattern"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'handle'"),
        };
        let pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'pattern'"),
        };
        let file_hint = call
            .args
            .get("file")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let context_lines = call
            .args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as usize;
        let limit = call
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        debug!(handle = %handle, pattern = %pattern, "context_grep tool");

        let store = self.store.lock().await;

        if !store.contains(&handle) {
            return ToolOutput::err(
                &call.id,
                format!(
                    "unknown handle '{}'. Use context_open to create a handle first.",
                    handle
                ),
            );
        }

        match store.grep(
            &handle,
            &pattern,
            file_hint.as_deref(),
            context_lines,
            limit,
        ) {
            Err(e) => ToolOutput::err(&call.id, format!("context_grep failed: {e}")),
            Ok(matches) if matches.is_empty() => {
                ToolOutput::ok(&call.id, format!("(no matches for '{}')", pattern))
            }
            Ok(matches) => {
                let mut lines: Vec<String> = Vec::new();
                lines.push(format!("{} match(es) for '{}':\n", matches.len(), pattern));
                for m in &matches {
                    let file_display = m
                        .file
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    for ctx in &m.context_before {
                        lines.push(format!("  {} | {}", m.line_number, ctx));
                    }
                    lines.push(format!("{}:L{}: {}", file_display, m.line_number, m.line));
                    for ctx in &m.context_after {
                        lines.push(format!("  {} | {}", m.line_number, ctx));
                    }
                    lines.push(String::new());
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_tool() -> (Arc<Mutex<ContextStore>>, ContextGrepTool) {
        let store = Arc::new(Mutex::new(ContextStore::new()));
        let tool = ContextGrepTool::new(store.clone());
        (store, tool)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "context_grep".into(),
            args,
        }
    }

    async fn open_tmp(store: &Arc<Mutex<ContextStore>>, content: &str) -> String {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        let meta = store.lock().await.open_file(tmp.path()).unwrap();
        std::mem::forget(tmp);
        meta.handle_id
    }

    #[tokio::test]
    async fn finds_matches() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "fn alpha() {}\nlet x = 1;\nfn beta() {}\n").await;
        let out = tool
            .execute(&call(json!({"handle": handle, "pattern": r"^fn "})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("2 match"), "{}", out.content);
        assert!(out.content.contains("alpha"), "{}", out.content);
        assert!(out.content.contains("beta"), "{}", out.content);
    }

    #[tokio::test]
    async fn no_matches_is_not_error() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "hello world\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "pattern": "xyzzy_not_found"}),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"), "{}", out.content);
    }

    #[tokio::test]
    async fn invalid_regex_is_error() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "test\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "pattern": "[invalid regex"}),
            ))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn missing_handle_is_error() {
        let (_store, tool) = make_tool();
        let out = tool
            .execute(&call(json!({"handle": "bad_handle", "pattern": "test"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown handle"));
    }
}
