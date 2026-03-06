// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::store::{BufferStatus, OutputBufferStore};

pub struct BufStatusTool {
    pub store: Arc<Mutex<OutputBufferStore>>,
}

impl BufStatusTool {
    pub fn new(store: Arc<Mutex<OutputBufferStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for BufStatusTool {
    fn name(&self) -> &str {
        "buf_status"
    }

    fn description(&self) -> &str {
        "Return the current status, line count, byte count, and elapsed time for a streaming \
         output buffer created by `task` or `shell`.\n\n\
         Use this tool to:\n\
         - Check whether a sub-agent has finished before reading its output.\n\
         - Poll a running sub-agent to see how many lines are available so far.\n\
         - Check the exit code after a sub-agent finishes.\n\n\
         Typical polling pattern:\n\
         ```\n\
         loop:\n\
           s = buf_status(handle)\n\
           if s.status == \"finished\": break\n\
           if s.lines > 50: buf_grep(handle, \"error\") -- check for problems early\n\
           wait and retry\n\
         ```"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "description": "Buffer handle ID returned by `task` or `shell`"
                }
            },
            "required": ["handle"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'handle'"),
        };

        let store = self.store.lock().await;

        match store.metadata(&handle) {
            None => ToolOutput::err(
                &call.id,
                format!(
                    "unknown buffer handle '{}'. Use `task` or `shell` to create a buffer first.",
                    handle
                ),
            ),
            Some(meta) => {
                let status_detail = match &meta.status {
                    BufferStatus::Running { pid } => match pid {
                        Some(p) => format!("running (pid {})", p),
                        None => "running".to_string(),
                    },
                    BufferStatus::Finished { exit_code } => {
                        format!("finished (exit code {})", exit_code)
                    }
                    BufferStatus::Failed { error } => format!("failed: {}", error),
                };

                let content = format!(
                    "Buffer: {handle}\n\
                     Description: {desc}\n\
                     Status: {status}\n\
                     Lines: {lines}\n\
                     Bytes: {bytes}\n\
                     Elapsed: {elapsed:.1}s",
                    handle = meta.handle_id,
                    desc = meta.description,
                    status = status_detail,
                    lines = meta.total_lines,
                    bytes = meta.total_bytes,
                    elapsed = meta.elapsed_secs,
                );

                ToolOutput::ok(&call.id, content)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::buffer::store::BufferSource;

    fn make_tool() -> (Arc<Mutex<OutputBufferStore>>, BufStatusTool) {
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        let tool = BufStatusTool::new(store.clone());
        (store, tool)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "buf_status".into(),
            args,
        }
    }

    #[tokio::test]
    async fn running_status() {
        let (store, tool) = make_tool();
        let id = {
            let mut s = store.lock().await;
            let id = s.create(BufferSource::Subagent {
                prompt: "test".into(),
                mode: "agent".into(),
                description: "test agent".into(),
            });
            s.append(&id, b"line1\nline2\n");
            id
        };
        let out = tool.execute(&call(json!({"handle": id}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("running"), "{}", out.content);
        assert!(out.content.contains("Lines: 2"), "{}", out.content);
    }

    #[tokio::test]
    async fn finished_status_shows_exit_code() {
        let (store, tool) = make_tool();
        let id = {
            let mut s = store.lock().await;
            let id = s.create(BufferSource::Subagent {
                prompt: "test".into(),
                mode: "agent".into(),
                description: "test agent".into(),
            });
            s.finish(&id, 0);
            id
        };
        let out = tool.execute(&call(json!({"handle": id}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("finished"), "{}", out.content);
        assert!(out.content.contains("exit code 0"), "{}", out.content);
    }

    #[tokio::test]
    async fn unknown_handle_is_error() {
        let (_store, tool) = make_tool();
        let out = tool.execute(&call(json!({"handle": "bad"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown buffer handle"));
    }
}
