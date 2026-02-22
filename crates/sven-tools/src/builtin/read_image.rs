// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput, ToolOutputPart};

pub struct ReadImageTool;

#[async_trait]
impl Tool for ReadImageTool {
    fn name(&self) -> &str { "read_image" }

    fn description(&self) -> &str {
        "Reads an image file and returns base64-encoded data URL for visual analysis.\n\n\
         ## Supported Formats\n\
         - PNG, JPEG, GIF, WebP, BMP, TIFF\n\
         - Automatically detects format\n\
         - Resizes large images (max 2048×2048)\n\n\
         ## Usage\n\
         - Visually inspect image files\n\
         - Analyze screenshots or diagrams\n\
         - Extract information from images\n\
         - Include images in analysis\n\n\
         ## When to Use\n\
         - Need to analyze image content visually\n\
         - Model supports image input (GPT-4V, Claude, etc.)\n\
         - Extracting text or information from screenshots\n\
         - Examining diagrams or visual data\n\n\
         ## When NOT to Use\n\
         - Model doesn't support images\n\
         - Need image metadata (use run_terminal_command + file tools)\n\
         - Converting formats (use appropriate tool)\n\n\
         ## Examples\n\
         <example>\n\
         Read and analyze screenshot:\n\
         read_image: path=\"/tmp/screenshot.png\"\n\
         </example>\n\
         <example>\n\
         Analyze diagram:\n\
         read_image: path=\"/project/docs/architecture.png\"\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Only works if model supports image input\n\
         - Large images auto-resized to 2048×2048 max\n\
         - Returns base64-encoded data URL\n\
         - Works with PNG, JPEG, GIF, WebP, BMP, TIFF\n\
         - Good for visual analysis and OCR tasks"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the image file"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path_str = match call.args.get("path").and_then(|v| v.as_str()) {
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

        debug!(path = %path_str, "read_image tool");

        // Validate extension before doing any I/O.
        let path = std::path::Path::new(&path_str);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !sven_image::is_image_extension(ext) {
            return ToolOutput::err(
                &call.id,
                format!(
                    "file does not appear to be an image (extension: .{ext}). \
                     Supported formats: png, jpg, jpeg, gif, webp, bmp, tiff."
                ),
            );
        }

        match sven_image::load_image(path) {
            Ok(img) => {
                let data_url = img.into_data_url();
                ToolOutput::with_parts(&call.id, vec![
                    ToolOutputPart::Text(format!("Image loaded: {path_str}")),
                    ToolOutputPart::Image(data_url),
                ])
            }
            Err(e) => ToolOutput::err(&call.id, format!("failed to read image: {e}")),
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::Tool;

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "ri1".into(), name: "read_image".into(), args }
    }

    /// Write a minimal 1×1 PNG to a temp file and return the path.
    fn tmp_png() -> String {
        // Minimal valid 1×1 red PNG (CRCs verified by Python zlib)
        let png_bytes: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a,
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
            0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41,
            0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
            0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92,
            0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
            0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let path = format!("/tmp/sven_read_image_test_{}.png", std::process::id());
        std::fs::write(&path, png_bytes).unwrap();
        path
    }

    #[tokio::test]
    async fn reads_png_returns_data_url() {
        let path = tmp_png();
        let t = ReadImageTool;
        let out = t.execute(&call(json!({"path": path}))).await;
        assert!(!out.is_error, "unexpected error: {}", out.content);
        assert!(out.has_images(), "should have an image part");
        // The image URL in parts should start with data:
        let img_part = out.parts.iter().find(|p| matches!(p, ToolOutputPart::Image(_)));
        assert!(img_part.is_some());
        if let Some(ToolOutputPart::Image(url)) = img_part {
            assert!(url.starts_with("data:image/"), "data url should start with data:image/");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_file_path_returns_error() {
        let t = ReadImageTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn non_image_extension_returns_error() {
        let t = ReadImageTool;
        let out = t.execute(&call(json!({"path": "/tmp/test.rs"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("does not appear to be an image"));
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let t = ReadImageTool;
        let out = t.execute(&call(json!({"path": "/tmp/no_such_image_xyz.png"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("failed to read image"));
    }
}
