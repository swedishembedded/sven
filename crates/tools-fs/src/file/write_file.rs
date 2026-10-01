// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_vocab::AgentMode;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;

/// The `write_file` tool. Its paths resolve through the [`PathScope`] it is built
/// with; [`Default`] is unconfined.
#[derive(Clone, Debug, Default)]
pub struct WriteTool {
    scope: PathScope,
}

impl WriteTool {
    /// Resolves its paths through `scope`.
    #[must_use]
    pub fn new(scope: PathScope) -> Self {
        Self { scope }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write a whole file. Overwrites by default; append=true extends instead.\n\
         Missing parent directories are created.\n\
         This replaces the entire contents, so it is the wrong call for changing part of\n\
         an existing file - that rewrite costs the whole file in tokens and loses anything\n\
         not restated.\n\
         Create files the user asked for. Do not add README, docs or config files\n\
         alongside them uninvited."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "text": {
                    "type": "string",
                    "description": "Text content to write to the file"
                },
                "append": {
                    "type": "boolean",
                    "description": "If true, append to existing content instead of overwriting (default false)"
                }
            },
            "required": ["path", "text", "append"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::WriteFile
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path_val = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let text_val = call
            .args
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let missing: Vec<_> = [("path", &path_val), ("text", &text_val)]
            .iter()
            .filter_map(|(name, v)| v.is_none().then_some(*name))
            .collect();
        if !missing.is_empty() {
            return ToolOutput::err(
                &call.id,
                format!(
                    "Missing required parameters: {}. Please provide all required parameters for this tool.",
                    missing.join(", ")
                ),
            );
        }

        let path = path_val.unwrap();
        let content = text_val.unwrap();
        let should_append = call
            .args
            .get("append")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        debug!(path = %path, append = should_append, "write tool");

        let target = match self.scope.resolve_for(call, &path) {
            Ok(p) => p,
            Err(refused) => return refused,
        };
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }

        if should_append {
            use tokio::io::AsyncWriteExt;
            match tokio::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&target)
                .await
            {
                Ok(mut f) => {
                    let result = f.write_all(content.as_bytes()).await;
                    // Explicitly flush + shutdown to ensure all bytes reach the OS before
                    // the file handle is dropped (tokio::fs::File close is async on drop).
                    let _ = f.flush().await;
                    let _ = f.shutdown().await;
                    match result {
                        Ok(_) => ToolOutput::ok(
                            &call.id,
                            format!("appended {} bytes to {path}", content.len()),
                        ),
                        Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
                    }
                }
                Err(e) => ToolOutput::err(&call.id, format!("open error: {e}")),
            }
        } else {
            match tokio::fs::write(&target, &content).await {
                Ok(_) => {
                    ToolOutput::ok(&call.id, format!("wrote {} bytes to {path}", content.len()))
                }
                Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
            }
        }
    }
}

impl ToolDisplay for WriteTool {
    fn display_name(&self) -> &str {
        "Write"
    }
    fn icon(&self) -> &str {
        "📝"
    }
    fn category(&self) -> &str {
        "file"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        sven_tool_api::tool_summary::tool_smart_summary("write_file", args)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "w1".into(),
            name: "write_file".into(),
            args,
        }
    }

    /// A scratch path unique to this process and call. The temp root comes
    /// from the environment (`TMPDIR`, honoured by `std::env::temp_dir`), never
    /// a hardcoded `/tmp`, so the suite also runs where `/tmp` is absent or
    /// read-only and inside a per-test sandbox.
    fn tmp_path() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("sven_write_test_{}_{n}.txt", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    #[tokio::test]
    async fn write_creates_file() {
        let path = tmp_path();
        let t = WriteTool::default();
        let out = t
            .execute(&call(json!({
                "path": path,
                "text": "hello write"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().trim(),
            "hello write"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn append_adds_to_file() {
        let path = tmp_path();
        let t = WriteTool::default();
        let w1 = t
            .execute(&call(json!({"path": path, "text": "first\n"})))
            .await;
        assert!(!w1.is_error, "write failed: {}", w1.content);
        let w2 = t
            .execute(&call(
                json!({"path": path, "text": "second\n", "append": true}),
            ))
            .await;
        assert!(!w2.is_error, "append failed: {}", w2.content);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            contents.contains("first"),
            "missing 'first' in: {contents:?}"
        );
        assert!(
            contents.contains("second"),
            "missing 'second' in: {contents:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("nested/sub/file.txt");
        let t = WriteTool::default();
        let out = t
            .execute(&call(json!({"path": path, "text": "nested"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(path.exists(), "parent directories should have been created");
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = WriteTool::default();
        let out = t.execute(&call(json!({"text": "x"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("Missing required parameters: path"));
    }

    #[tokio::test]
    async fn missing_content_is_error() {
        let t = WriteTool::default();
        let out = t.execute(&call(json!({"path": "x.txt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("Missing required parameters: text"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = WriteTool::default();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn null_path_is_error() {
        let t = WriteTool::default();
        let out = t.execute(&call(json!({"path": null, "text": "x"}))).await;
        assert!(out.is_error, "null path should be an error");
    }

    #[tokio::test]
    async fn integer_path_is_error() {
        let t = WriteTool::default();
        let out = t.execute(&call(json!({"path": 42, "text": "x"}))).await;
        assert!(out.is_error, "integer path should be an error");
    }

    #[tokio::test]
    async fn path_traversal_does_not_crash() {
        let t = WriteTool::default();
        // The tool should either write to the traversed path or error cleanly,
        // but must not panic. The `..` segments are the point of the test, so
        // they stay; the whole path is rooted in a temp dir that is removed
        // afterwards whatever the tool decided to do with them.
        let dir = tempfile::tempdir().expect("create temp dir");
        let traversed = dir.path().join("a/../b/out.txt");
        let out = t
            .execute(&call(json!({"path": traversed, "text": "traversal"})))
            .await;
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn extremely_large_content_does_not_panic() {
        let path = tmp_path();
        let t = WriteTool::default();
        let large_text = "x".repeat(10_000_000);
        let out = t
            .execute(&call(json!({"path": path, "text": large_text})))
            .await;
        let _ = out.is_error;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_preserves_unicode_content() {
        let path = tmp_path();
        let t = WriteTool::default();
        let content = "Unicode: café 中文 日本語 한국어 🎉";
        let out = t
            .execute(&call(json!({"path": path, "text": content})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, content,
            "multi-byte UTF-8 characters must survive the write/read roundtrip"
        );
        let _ = std::fs::remove_file(&path);
    }
}
