// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::store::OutputBufferStore;

pub struct BufGrepTool {
    pub(crate) store: Arc<Mutex<OutputBufferStore>>,
}

impl BufGrepTool {
    pub fn new(store: Arc<Mutex<OutputBufferStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for BufGrepTool {
    fn name(&self) -> &str {
        "buf_grep"
    }

    fn description(&self) -> &str {
        "Search a streaming output buffer for lines matching a regex pattern.  Works the same \
         as `context_grep` but operates on a buffer created by `task` or `shell`.\n\n\
         The buffer may still be growing (status: running).  Grepping while running is safe — \
         you get matches against all bytes appended so far.\n\n\
         Use this to quickly locate errors, test results, or specific identifiers in a \
         sub-agent's output without reading the entire buffer.\n\n\
         Supports full Rust regex syntax.  Use (?i) prefix for case-insensitive matching."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Buffer handle ID returned by `task` or `shell`"
                },
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for (Rust regex syntax). \
                                    Use (?i) for case-insensitive."
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

        let store = self.store.lock().await;

        if !store.contains(&handle) {
            return ToolOutput::err(
                &call.id,
                format!(
                    "unknown buffer handle '{}'. Use `task` or `shell` to create a buffer first.",
                    handle
                ),
            );
        }

        match store.grep(&handle, &pattern, context_lines, limit) {
            Err(e) => ToolOutput::err(&call.id, format!("buf_grep failed: {e}")),
            Ok(matches) if matches.is_empty() => {
                ToolOutput::ok(&call.id, format!("(no matches for '{}')", pattern))
            }
            Ok(matches) => {
                let mut lines: Vec<String> = Vec::new();
                lines.push(format!("{} match(es) for '{}':\n", matches.len(), pattern));
                for m in &matches {
                    for ctx in &m.context_before {
                        lines.push(format!("  {} | {}", m.line_number, ctx));
                    }
                    lines.push(format!("<buffer>:L{}: {}", m.line_number, m.line));
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
    use crate::builtin::buffer::store::BufferSource;

    fn make_tool() -> (Arc<Mutex<OutputBufferStore>>, BufGrepTool) {
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        let tool = BufGrepTool::new(store.clone());
        (store, tool)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "buf_grep".into(),
            args,
        }
    }

    async fn create_buf(store: &Arc<Mutex<OutputBufferStore>>, content: &str) -> String {
        let mut s = store.lock().await;
        let id = s.create(BufferSource::Subagent {
            prompt: "test".into(),
            mode: "agent".into(),
            description: "test".into(),
        });
        s.append(&id, content.as_bytes());
        id
    }

    #[tokio::test]
    async fn finds_matches() {
        let (store, tool) = make_tool();
        let handle = create_buf(&store, "fn alpha() {}\nlet x = 1;\nfn beta() {}\n").await;
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
        let handle = create_buf(&store, "hello world\n").await;
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
        let handle = create_buf(&store, "test\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "pattern": "[invalid regex"}),
            ))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn unknown_handle_is_error() {
        let (_store, tool) = make_tool();
        let out = tool
            .execute(&call(json!({"handle": "bad", "pattern": "x"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown buffer handle"));
    }
}
