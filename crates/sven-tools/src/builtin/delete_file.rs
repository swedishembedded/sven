// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct DeleteFileTool;

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }

    fn description(&self) -> &str {
        "Delete a single file. Fails gracefully if not found. NEVER delete without explicit user request.\n\
         Permanent — no recovery. For directories use run_terminal_command with rm -r."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to delete"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let args_preview =
                    serde_json::to_string(&call.args).unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "missing required parameter 'path'. Received: {}",
                        args_preview
                    ),
                );
            }
        };

        debug!(path = %path, "delete_file tool");

        // Refuse to delete directories
        match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_dir() => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "{path} is a directory; use run_terminal_command with 'rm -rf' instead"
                    ),
                );
            }
            Err(e) => return ToolOutput::err(&call.id, format!("stat error: {e}")),
            Ok(_) => {}
        }

        match tokio::fs::remove_file(&path).await {
            Ok(_) => ToolOutput::ok(&call.id, format!("deleted {path}")),
            Err(e) => ToolOutput::err(&call.id, format!("delete error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "d1".into(),
            name: "delete_file".into(),
            args,
        }
    }

    #[tokio::test]
    async fn deletes_existing_file() {
        let path = {
            use std::sync::atomic::{AtomicU32, Ordering};
            static CTR: AtomicU32 = AtomicU32::new(0);
            let n = CTR.fetch_add(1, Ordering::Relaxed);
            format!("/tmp/sven_delete_test_{}_{n}.txt", std::process::id())
        };
        std::fs::write(&path, "bye").unwrap();
        let t = DeleteFileTool;
        let out = t.execute(&call(json!({"path": path}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("deleted"));
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let t = DeleteFileTool;
        let out = t
            .execute(&call(json!({"path": "/tmp/sven_no_such_delete_xyz.txt"})))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn directory_is_error() {
        let t = DeleteFileTool;
        let out = t.execute(&call(json!({"path": "/tmp"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("directory"));
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = DeleteFileTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = DeleteFileTool;
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }
}
