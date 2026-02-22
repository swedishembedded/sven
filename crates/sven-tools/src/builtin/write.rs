// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str { "write" }

    fn description(&self) -> &str {
        "Writes a file to the local filesystem. This tool will overwrite the existing file if \
         one exists at the provided path. ALWAYS prefer editing existing files with edit_file. \
         NEVER write new files unless explicitly required. \
         NEVER proactively create documentation or README files unless explicitly requested. \
         Creates parent directories automatically. \
         Set append=true to add to the end of an existing file instead of overwriting."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append to existing content instead of overwriting (default false)"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'path'. Received: {}", args_preview)
                );
            }
        };
        let content = match call.args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'content'. Received: {}", args_preview)
                );
            }
        };
        let should_append = call.args.get("append").and_then(|v| v.as_bool()).unwrap_or(false);

        debug!(path = %path, append = should_append, "write tool");

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }

        if should_append {
            use tokio::io::AsyncWriteExt;
            match tokio::fs::OpenOptions::new().append(true).create(true).open(&path).await {
                Ok(mut f) => {
                    let result = f.write_all(content.as_bytes()).await;
                    // Explicitly flush + shutdown to ensure all bytes reach the OS before
                    // the file handle is dropped (tokio::fs::File close is async on drop).
                    let _ = f.flush().await;
                    let _ = f.shutdown().await;
                    match result {
                        Ok(_) => ToolOutput::ok(&call.id, format!("appended {} bytes to {path}", content.len())),
                        Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
                    }
                }
                Err(e) => ToolOutput::err(&call.id, format!("open error: {e}")),
            }
        } else {
            match tokio::fs::write(&path, &content).await {
                Ok(_) => ToolOutput::ok(&call.id, format!("wrote {} bytes to {path}", content.len())),
                Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "w1".into(), name: "write".into(), args }
    }

    fn tmp_path() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        format!("/tmp/sven_write_test_{}_{n}.txt", std::process::id())
    }

    #[tokio::test]
    async fn write_creates_file() {
        let path = tmp_path();
        let t = WriteTool;
        let out = t.execute(&call(json!({
            "path": path,
            "content": "hello write"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "hello write");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn append_adds_to_file() {
        let path = tmp_path();
        let t = WriteTool;
        let w1 = t.execute(&call(json!({"path": path, "content": "first\n"}))).await;
        assert!(!w1.is_error, "write failed: {}", w1.content);
        let w2 = t.execute(&call(json!({"path": path, "content": "second\n", "append": true}))).await;
        assert!(!w2.is_error, "append failed: {}", w2.content);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("first"), "missing 'first' in: {contents:?}");
        assert!(contents.contains("second"), "missing 'second' in: {contents:?}");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let dir = format!("/tmp/sven_write_nested_{}_{n}", std::process::id());
        let path = format!("{dir}/sub/file.txt");
        let t = WriteTool;
        let out = t.execute(&call(json!({"path": path, "content": "nested"}))).await;
        assert!(!out.is_error, "{}", out.content);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = WriteTool;
        let out = t.execute(&call(json!({"content": "x"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn missing_content_is_error() {
        let t = WriteTool;
        let out = t.execute(&call(json!({"path": "/tmp/x.txt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'content'"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = WriteTool;
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }
}
