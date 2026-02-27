// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

const READ_LIMIT: usize = 200_000;

/// Built-in tool for file-system read/write operations.
pub struct FsTool;

#[async_trait]
impl Tool for FsTool {
    fn name(&self) -> &str { "fs" }

    fn description(&self) -> &str {
        "Read, write, or list files. Operations: read, write, append, list."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["read", "write", "append", "list"],
                    "description": "File system operation"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory path"
                },
                "text": {
                    "type": "string",
                    "description": "Text content to write (required for write/append - set as empty for others)"
                }
            },
            "required": ["operation", "path", "text"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }
    fn output_category(&self) -> OutputCategory { OutputCategory::FileContent }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let op = match call.args.get("operation").and_then(|v| v.as_str()) {
            Some(o) => o.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'operation'"),
        };
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'path'"),
        };

        debug!(op = %op, path = %path, "fs tool");

        match op.as_str() {
            "read" => {
                match tokio::fs::read(&path).await {
                    Ok(bytes) => {
                        let text = String::from_utf8_lossy(&bytes);
                        let content = if text.len() > READ_LIMIT {
                            format!("{}...[truncated]", &text[..READ_LIMIT])
                        } else {
                            text.to_string()
                        };
                        ToolOutput::ok(&call.id, content)
                    }
                    Err(e) => ToolOutput::err(&call.id, format!("read error: {e}")),
                }
            }
            "write" => {
                let content = match call.args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return ToolOutput::err(
                        &call.id,
                        "write requires a 'content' field but it is missing. \
                         This usually means the JSON was truncated because the content was too \
                         large to fit in a single generation.",
                    ),
                };
                let truncated = call.args.get("__truncated").and_then(|v| v.as_bool()).unwrap_or(false);
                // Create parent directories if needed
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                }
                match tokio::fs::write(&path, content).await {
                    Ok(_) if truncated => ToolOutput::ok(
                        &call.id,
                        format!(
                            "Partial write: wrote {} bytes to {path}. \
                             The output was cut off by the token limit — this is not the complete \
                             content. Use the `append` operation to add the remaining content.",
                            content.len()
                        ),
                    ),
                    Ok(_) => ToolOutput::ok(&call.id, format!("wrote {} bytes to {path}", content.len())),
                    Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
                }
            }
            "append" => {
                use tokio::io::AsyncWriteExt;
                let content = match call.args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => return ToolOutput::err(
                        &call.id,
                        "append requires a 'content' field but it is missing. \
                         This usually means the JSON was truncated because the content was too \
                         large to fit in a single generation.",
                    ),
                };
                let truncated = call.args.get("__truncated").and_then(|v| v.as_bool()).unwrap_or(false);
                match tokio::fs::OpenOptions::new().append(true).create(true).open(&path).await {
                    Ok(mut f) => match f.write_all(content.as_bytes()).await {
                        Ok(_) if truncated => ToolOutput::ok(
                            &call.id,
                            format!(
                                "Partial append: wrote {} bytes to {path}. \
                                 The output was cut off by the token limit. \
                                 Use another `append` call to add the remaining content.",
                                content.len()
                            ),
                        ),
                        Ok(_) => ToolOutput::ok(&call.id, format!("appended {} bytes to {path}", content.len())),
                        Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
                    },
                    Err(e) => ToolOutput::err(&call.id, format!("open error: {e}")),
                }
            }
            "list" => {
                match tokio::fs::read_dir(&path).await {
                    Ok(mut rd) => {
                        let mut entries = Vec::new();
                        while let Ok(Some(entry)) = rd.next_entry().await {
                            let name = entry.file_name().to_string_lossy().to_string();
                            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
                            entries.push(if is_dir { format!("{name}/") } else { name });
                        }
                        entries.sort();
                        ToolOutput::ok(&call.id, entries.join("\n"))
                    }
                    Err(e) => ToolOutput::err(&call.id, format!("list error: {e}")),
                }
            }
            other => ToolOutput::err(&call.id, format!("unknown operation: {other}")),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: id.into(), name: "fs".into(), args }
    }

    fn tmp_path() -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        format!("/tmp/sven_fs_test_{}_{n}.txt", std::process::id())
    }

    // ── write + read round-trip ───────────────────────────────────────────────

    #[tokio::test]
    async fn write_then_read_roundtrip() {
        let path = tmp_path();
        let t = FsTool;

        let w = t.execute(&call("w", json!({
            "operation": "write",
            "path": path,
            "content": "hello fs"
        }))).await;
        assert!(!w.is_error, "write failed: {}", w.content);

        let r = t.execute(&call("r", json!({
            "operation": "read",
            "path": path
        }))).await;
        assert!(!r.is_error);
        assert_eq!(r.content.trim(), "hello fs");

        let _ = std::fs::remove_file(&path);
    }

    // ── append ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn append_adds_to_existing_content() {
        let path = tmp_path();
        let t = FsTool;

        t.execute(&call("w", json!({"operation":"write","path":path,"content":"line1\n"}))).await;
        t.execute(&call("a", json!({"operation":"append","path":path,"content":"line2\n"}))).await;

        let r = t.execute(&call("r", json!({"operation":"read","path":path}))).await;
        assert!(r.content.contains("line1"));
        assert!(r.content.contains("line2"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn append_creates_file_if_missing() {
        let path = tmp_path();
        let t = FsTool;

        let a = t.execute(&call("a", json!({
            "operation": "append",
            "path": path,
            "content": "created"
        }))).await;
        assert!(!a.is_error);

        let r = t.execute(&call("r", json!({"operation":"read","path":path}))).await;
        assert!(r.content.contains("created"));

        let _ = std::fs::remove_file(&path);
    }

    // ── write creates parent dirs ─────────────────────────────────────────────

    #[tokio::test]
    async fn write_creates_nested_directories() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR1: AtomicU32 = AtomicU32::new(0);
        let dir = format!("/tmp/sven_nested_{}_{}", std::process::id(), CTR1.fetch_add(1, Ordering::Relaxed));
        let path = format!("{dir}/sub/file.txt");
        let t = FsTool;

        let w = t.execute(&call("w", json!({
            "operation": "write",
            "path": path,
            "content": "nested"
        }))).await;
        assert!(!w.is_error, "{}", w.content);

        let r = t.execute(&call("r", json!({"operation":"read","path":path}))).await;
        assert!(r.content.contains("nested"));

        let _ = std::fs::remove_dir_all(dir);
    }

    // ── list ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_directory_returns_entries() {
        let t = FsTool;
        let r = t.execute(&call("l", json!({"operation":"list","path":"/tmp"}))).await;
        assert!(!r.is_error);
        assert!(!r.content.is_empty());
    }

    #[tokio::test]
    async fn list_shows_slash_for_directories() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR2: AtomicU32 = AtomicU32::new(0);
        let dir = format!("/tmp/sven_listdir_{}_{}", std::process::id(), CTR2.fetch_add(1, Ordering::Relaxed));
        std::fs::create_dir_all(format!("{dir}/subdir")).unwrap();
        std::fs::write(format!("{dir}/file.txt"), "x").unwrap();

        let t = FsTool;
        let r = t.execute(&call("l", json!({"operation":"list","path":dir}))).await;
        assert!(r.content.contains("subdir/"), "dirs should have trailing slash");
        assert!(r.content.contains("file.txt"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── missing content for write/append is an error ─────────────────────────

    #[tokio::test]
    async fn write_without_content_is_error() {
        let path = tmp_path();
        let t = FsTool;
        let r = t.execute(&call("w", json!({
            "operation": "write",
            "path": path
        }))).await;
        assert!(r.is_error, "expected error when content is missing");
        assert!(r.content.contains("content"), "error should mention the missing field");
        // File must not have been created
        assert!(!std::path::Path::new(&path).exists(), "no file should be created");
    }

    #[tokio::test]
    async fn append_without_content_is_error() {
        let path = tmp_path();
        let t = FsTool;
        let r = t.execute(&call("a", json!({
            "operation": "append",
            "path": path
        }))).await;
        assert!(r.is_error, "expected error when content is missing");
        assert!(r.content.contains("content"), "error should mention the missing field");
    }

    // ── Error cases ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_missing_file_is_error() {
        let t = FsTool;
        let r = t.execute(&call("r", json!({
            "operation": "read",
            "path": "/tmp/sven_does_not_exist_xyz.txt"
        }))).await;
        assert!(r.is_error);
        assert!(r.content.contains("read error"));
    }

    #[tokio::test]
    async fn missing_operation_is_error() {
        let t = FsTool;
        let r = t.execute(&call("e", json!({"path":"/tmp/x"}))).await;
        assert!(r.is_error);
        assert!(r.content.contains("missing 'operation'"));
    }

    #[tokio::test]
    async fn missing_path_is_error() {
        let t = FsTool;
        let r = t.execute(&call("e", json!({"operation":"read"}))).await;
        assert!(r.is_error);
        assert!(r.content.contains("missing 'path'"));
    }

    #[tokio::test]
    async fn unknown_operation_is_error() {
        let t = FsTool;
        let r = t.execute(&call("e", json!({"operation":"delete","path":"/tmp/x"}))).await;
        assert!(r.is_error);
        assert!(r.content.contains("unknown operation"));
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_requires_operation_and_path() {
        let t = FsTool;
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        let names: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"operation"));
        assert!(names.contains(&"path"));
    }
}
