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

pub struct BufReadTool {
    pub(crate) store: Arc<Mutex<OutputBufferStore>>,
}

impl BufReadTool {
    pub fn new(store: Arc<Mutex<OutputBufferStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for BufReadTool {
    fn name(&self) -> &str {
        "buf_read"
    }

    fn description(&self) -> &str {
        "Read a specific line range from a streaming output buffer created by the `task` or \
         `shell` tool.  Returns lines formatted as `L{n}:{content}` — the same format as \
         `context_read`.\n\n\
         The buffer may still be growing (status: running).  Reading while running is safe; \
         you get a snapshot of all bytes appended so far.\n\n\
         Typical workflow after spawning a sub-agent with `task`:\n\
         1. `buf_status` to check whether the sub-agent is still running and how many lines are available.\n\
         2. `buf_grep` to locate specific sections of the output.\n\
         3. `buf_read` to inspect specific line ranges in detail.\n\n\
         You do NOT need to read the entire buffer — grep for what you need first."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Buffer handle ID returned by `task` or `shell`"
                },
                "start_line": {
                    "type": "integer",
                    "description": "First line to read (1-indexed, inclusive)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "Last line to read (1-indexed, inclusive)"
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

        if start_line < 1 {
            return ToolOutput::err(&call.id, "start_line must be >= 1");
        }
        if end_line < start_line {
            return ToolOutput::err(&call.id, "end_line must be >= start_line");
        }

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

        match store.read_range(&handle, start_line, end_line) {
            Ok(text) if text.is_empty() => ToolOutput::ok(
                &call.id,
                "(buffer is empty or range is beyond current content)",
            ),
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, format!("buf_read failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::buffer::store::BufferSource;

    fn make_tool() -> (Arc<Mutex<OutputBufferStore>>, BufReadTool) {
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        let tool = BufReadTool::new(store.clone());
        (store, tool)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "buf_read".into(),
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
    async fn reads_specific_range() {
        let (store, tool) = make_tool();
        let handle = create_buf(&store, "alpha\nbeta\ngamma\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "start_line": 2, "end_line": 2}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("L2:beta"), "{}", out.content);
        assert!(!out.content.contains("alpha"), "{}", out.content);
    }

    #[tokio::test]
    async fn unknown_handle_is_error() {
        let (_store, tool) = make_tool();
        let out = tool
            .execute(&call(
                json!({"handle": "bad", "start_line": 1, "end_line": 5}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown buffer handle"));
    }

    #[tokio::test]
    async fn invalid_range_is_error() {
        let (store, tool) = make_tool();
        let handle = create_buf(&store, "line\n").await;
        let out = tool
            .execute(&call(
                json!({"handle": handle, "start_line": 5, "end_line": 3}),
            ))
            .await;
        assert!(out.is_error);
    }
}
