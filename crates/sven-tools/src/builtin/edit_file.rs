// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }

    fn description(&self) -> &str {
        "Performs exact string replacements in files.\n\n\
         ## Usage\n\
         - Edit FAILS if old_str not found or not unique in file\n\
         - Provide 2-5 lines of surrounding context for uniqueness\n\
         - Include exact whitespace and indentation as it appears in the file\n\
         - For renaming symbols throughout a single file, use replace_all=true\n\n\
         ## When to Use\n\
         - Modifying existing code with surrounding context known\n\
         - Making surgical changes to specific locations\n\
         - Renaming symbols in a single file (with replace_all=true)\n\n\
         ## When NOT to Use\n\
         - Creating new files → use write tool instead\n\
         - Making changes to multiple files → call edit_file multiple times\n\
         - Large-scale refactoring → consider apply_patch tool\n\n\
         ## How to Succeed\n\
         1. ALWAYS read the file first with read_file to see exact formatting\n\
         2. Copy 2-5 lines surrounding your target change\n\
         3. Include exact indentation and all whitespace\n\
         4. Ensure old_str appears exactly once in the file (check carefully)\n\
         5. If edit fails with 'not found', copy MORE context lines\n\
         6. If edit fails with 'appears N times', add UNIQUE context\n\n\
         ## Examples\n\
         <example>\n\
         Good - includes 3 lines of context:\n\
         edit_file(\n\
           path=\"src/main.rs\",\n\
           old_str=\"fn main() {\\n    println!(\\\"Hello\\\");\\n}\",\n\
           new_str=\"fn main() {\\n    println!(\\\"Hello, World!\\\");\\n}\"\n\
         )\n\
         </example>\n\
         <example>\n\
         Bad - insufficient context (will fail if println! appears elsewhere):\n\
         edit_file(path=\"src/main.rs\", old_str=\"println!\", new_str=\"eprintln!\")\n\
         </example>\n\
         <example>\n\
         Good workflow - read first, then edit:\n\
         1. read_file: path=\"src/processor.rs\" → See exact formatting\n\
         2. edit_file with 2-5 lines of context from the read output\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - ALWAYS read the file first to see exact formatting\n\
         - Include 2-5 context lines above and below your change\n\
         - Respect existing indentation precisely\n\
         - For new files, use write tool\n\
         - Parent directories are created automatically if needed"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file"
                },
                "old_str": {
                    "type": "string",
                    "description": "Exact string to find and replace (must be unique in the file)"
                },
                "new_str": {
                    "type": "string",
                    "description": "Replacement string"
                }
            },
            "required": ["path", "old_str", "new_str"],
            "additionalProperties": false
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
        let old_str = match call.args.get("old_str").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'old_str'. Received: {}", args_preview)
                );
            }
        };
        let new_str = match call.args.get("new_str").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                let args_preview = serde_json::to_string(&call.args)
                    .unwrap_or_else(|_| "null".to_string());
                return ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'new_str'. Received: {}", args_preview)
                );
            }
        };

        debug!(path = %path, "edit_file tool");

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(&call.id, format!("read error: {e}")),
        };

        let count = content.matches(&old_str as &str).count();
        if count == 0 {
            return ToolOutput::err(
                &call.id,
                format!("old_str not found in {path}"),
            );
        }
        if count > 1 {
            return ToolOutput::err(
                &call.id,
                format!("old_str appears {count} times in {path}; provide more context to make it unique"),
            );
        }

        let new_content = content.replacen(&old_str as &str, &new_str, 1);

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }

        match tokio::fs::write(&path, &new_content).await {
            Ok(_) => ToolOutput::ok(&call.id, format!("edited {path}")),
            Err(e) => ToolOutput::err(&call.id, format!("write error: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "e1".into(), name: "edit_file".into(), args }
    }

    fn tmp_file(content: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_edit_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, content).unwrap();
        path
    }

    #[tokio::test]
    async fn replaces_unique_string() {
        let path = tmp_file("hello world\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "world",
            "new_str": "rust"
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "hello rust\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fails_if_not_found() {
        let path = tmp_file("hello world\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "xyz",
            "new_str": "abc"
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fails_if_ambiguous() {
        let path = tmp_file("foo foo foo\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "foo",
            "new_str": "bar"
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("3 times"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_file_path_is_error() {
        let t = EditFileTool;
        let out = t.execute(&call(json!({"old_str": "a", "new_str": "b"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = EditFileTool;
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }
}
