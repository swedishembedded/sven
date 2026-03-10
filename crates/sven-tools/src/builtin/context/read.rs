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

pub struct ContextReadTool {
    pub(crate) store: Arc<Mutex<ContextStore>>,
}

impl ContextReadTool {
    pub fn new(store: Arc<Mutex<ContextStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ContextReadTool {
    fn name(&self) -> &str {
        "context_read"
    }

    fn description(&self) -> &str {
        "Read a specific line range from a memory-mapped context handle. Only the requested range \
         is returned — your context window stays efficient.\n\n\
         Use this after context_open to:\n\
         - Inspect sections identified by context_grep matches\n\
         - Read file headers, imports, or struct definitions\n\
         - Check specific functions or code blocks\n\n\
         For directory handles, pass 'file' to select a specific file by path substring. \
         Without 'file', the directory's files are treated as concatenated and lines are \
         numbered globally.\n\n\
         For ranges that exceed the tool output cap, narrow your range or use context_query \
         to analyze the section with a sub-agent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Context handle ID returned by context_open"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to read (1-indexed, inclusive)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to read (1-indexed, inclusive)"
                },
                "file": {
                    "type": "string",
                    "description": "For directory handles: path substring to select a specific file"
                }
            },
            "required": ["handle", "start_line", "end_line"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::FileContent
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'handle'"),
        };
        let start_line = match call.args.get("start_line").and_then(|v| v.as_u64()) {
            Some(n) => n as usize,
            None => return ToolOutput::err(&call.id, "missing required parameter 'start_line'"),
        };
        let end_line = match call.args.get("end_line").and_then(|v| v.as_u64()) {
            Some(n) => n as usize,
            None => return ToolOutput::err(&call.id, "missing required parameter 'end_line'"),
        };
        let file_hint = call
            .args
            .get("file")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        debug!(handle = %handle, start_line, end_line, "context_read tool");

        if start_line == 0 {
            return ToolOutput::err(&call.id, "start_line must be >= 1 (1-indexed)");
        }
        if end_line < start_line {
            return ToolOutput::err(
                &call.id,
                format!(
                    "end_line ({}) must be >= start_line ({})",
                    end_line, start_line
                ),
            );
        }

        let store = self.store.lock().await;

        // Validate handle existence.
        if !store.contains(&handle) {
            return ToolOutput::err(
                &call.id,
                format!(
                    "unknown handle '{}'. Use context_open to create a handle first.",
                    handle
                ),
            );
        }

        match store.read_range(&handle, start_line, end_line, file_hint.as_deref()) {
            Ok(text) => {
                if text.is_empty() {
                    ToolOutput::ok(&call.id, "(empty range)")
                } else {
                    ToolOutput::ok(&call.id, text)
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("context_read failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_tool() -> (Arc<Mutex<ContextStore>>, ContextReadTool) {
        let store = Arc::new(Mutex::new(ContextStore::new()));
        let tool = ContextReadTool::new(store.clone());
        (store, tool)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "context_read".into(),
            args,
        }
    }

    async fn open_tmp(store: &Arc<Mutex<ContextStore>>, content: &str) -> String {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(content.as_bytes()).unwrap();
        tmp.flush().unwrap();
        let meta = store.lock().await.open_file(tmp.path()).unwrap();
        // Keep tmp alive by leaking it — acceptable in tests.
        std::mem::forget(tmp);
        meta.handle_id
    }

    #[tokio::test]
    async fn reads_correct_range() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "alpha\nbeta\ngamma\ndelta\n").await;
        let out = tool
            .execute(&call(json!({
                "handle": handle,
                "start_line": 2,
                "end_line": 3
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("L2:beta"), "{}", out.content);
        assert!(out.content.contains("L3:gamma"), "{}", out.content);
        assert!(!out.content.contains("L1:"));
        assert!(!out.content.contains("L4:"));
    }

    #[tokio::test]
    async fn missing_handle_returns_error() {
        let (_store, tool) = make_tool();
        let out = tool
            .execute(&call(
                json!({"handle": "bad", "start_line": 1, "end_line": 5}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown handle"));
    }

    #[tokio::test]
    async fn zero_start_line_returns_error() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "x\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "start_line": 0, "end_line": 1}),
            ))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn end_before_start_returns_error() {
        let (store, tool) = make_tool();
        let handle = open_tmp(&store, "x\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "start_line": 5, "end_line": 2}),
            ))
            .await;
        assert!(out.is_error);
    }
}
