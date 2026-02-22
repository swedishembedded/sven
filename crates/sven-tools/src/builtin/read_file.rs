use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput, ToolOutputPart};

const READ_LIMIT: usize = 200_000;

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }

    fn description(&self) -> &str {
        "Reads a file from the local filesystem. You can access any file directly by using \
         this tool. It is okay to read a file that does not exist; an error will be returned. \
         Optionally specify a line offset and limit for large files, but it is recommended to \
         read the whole file when possible. Lines in the output are numbered starting at 1. \
         If the file exists but has empty contents, 'File is empty.' is returned. \
         Image files (png, jpg, jpeg, gif, webp, bmp, tiff) are automatically returned as \
         base64-encoded data URLs when the model supports image input."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "offset": {
                    "type": "integer",
                    "description": "1-indexed line number to start reading from (default 1)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default 2000)"
                }
            },
            "required": ["path"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

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
        let offset = call.args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = call.args.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        debug!(path = %path, offset, limit, "read_file tool");

        // Auto-detect image files and return them as data URLs.
        let ext = std::path::Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if sven_image::is_image_extension(ext) {
            return match sven_image::load_image(std::path::Path::new(&path)) {
                Ok(img) => {
                    let data_url = img.into_data_url();
                    ToolOutput::with_parts(&call.id, vec![
                        ToolOutputPart::Text(format!("Image file: {path}")),
                        ToolOutputPart::Image(data_url),
                    ])
                }
                Err(e) => ToolOutput::err(&call.id, format!("failed to read image: {e}")),
            };
        }

        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let capped = if text.len() > READ_LIMIT {
                    format!("{}...[file truncated at {} bytes]", &text[..READ_LIMIT], text.len())
                } else {
                    text.to_string()
                };

                let start = offset.saturating_sub(1);
                let lines: Vec<&str> = capped.lines().collect();
                let total = lines.len();

                let selected: Vec<String> = lines
                    .into_iter()
                    .enumerate()
                    .skip(start)
                    .take(limit)
                    .map(|(i, line)| format!("L{}:{}", i + 1, line))
                    .collect();

                let mut content = selected.join("\n");
                let shown = limit.min(total.saturating_sub(start));
                if start + shown < total {
                    content.push_str(&format!(
                        "\n...[{} more lines, use offset={} to continue]",
                        total - start - shown,
                        start + shown + 1
                    ));
                }

                ToolOutput::ok(&call.id, content)
            }
            Err(e) => ToolOutput::err(&call.id, format!("read error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "r1".into(), name: "read_file".into(), args }
    }

    fn tmp_file(content: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_read_file_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn reads_file_with_line_numbers() {
        let path = tmp_file("alpha\nbeta\ngamma\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": path}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("L1:alpha"));
        assert!(out.content.contains("L2:beta"));
        assert!(out.content.contains("L3:gamma"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn offset_and_limit_work() {
        let path = tmp_file("line1\nline2\nline3\nline4\nline5\n");
        let t = ReadFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "offset": 2,
            "limit": 2
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("L2:line2"));
        assert!(out.content.contains("L3:line3"));
        assert!(!out.content.contains("L1:"));
        assert!(!out.content.contains("L4:"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_file_is_error() {
        let t = ReadFileTool;
        let out = t.execute(&call(json!({"path": "/tmp/sven_no_such_file_xyz.txt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("read error"));
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = ReadFileTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }
}
