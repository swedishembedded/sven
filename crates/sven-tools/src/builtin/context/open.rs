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

pub struct ContextOpenTool {
    pub store: Arc<Mutex<ContextStore>>,
}

impl ContextOpenTool {
    pub fn new(store: Arc<Mutex<ContextStore>>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for ContextOpenTool {
    fn name(&self) -> &str {
        "context_open"
    }

    fn description(&self) -> &str {
        "Open a file or directory as a memory-mapped context for efficient analysis of content \
         too large to fit in your context window. Returns a handle and structural metadata — \
         the content itself is NOT loaded into your context.\n\n\
         After opening, interact with the handle using:\n\
         - context_read: peek at specific line ranges (random access, zero-copy)\n\
         - context_grep: search for patterns (cheap pre-filter before deeper analysis)\n\
         - context_query: dispatch sub-agent analysis over chunks\n\n\
         Recommended strategy:\n\
         1. Open to get structure and size\n\
         2. Grep to locate relevant sections (cheap, keeps context window clean)\n\
         3. Read specific sections you need to understand directly\n\
         4. Query chunks when semantic analysis of many sections is needed\n\n\
         For files under ~500 lines prefer read_file — it is simpler and sufficient.\n\
         For build logs, CI output, large codebases, or any content over ~1000 lines, \
         always use context_open."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the file or directory to open"
                },
                "include_pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files in a directory (e.g. '*.rs', '*.c'). \
                                    Omit to include all text files."
                },
                "recursive": {
                    "type": "boolean",
                    "description": "Recursively include subdirectories (default: true)"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::Generic
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path_str = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'path'"),
        };
        let include_pattern = call
            .args
            .get("include_pattern")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let recursive = call
            .args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        debug!(path = %path_str, "context_open tool");

        let path = std::path::Path::new(&path_str);

        let result = {
            let mut store = self.store.lock().await;
            if path.is_dir() {
                store.open_directory(path, include_pattern.as_deref(), recursive)
            } else {
                store.open_file(path)
            }
        };

        match result {
            Ok(meta) => {
                let output = format!(
                    "Context opened: handle={}\n\
                     Files: {}, Lines: {}, Bytes: {}\n\n\
                     {}\n\n\
                     Use context_grep(handle=\"{}\", ...) to locate relevant sections.\n\
                     Use context_read(handle=\"{}\", start_line=N, end_line=M) to inspect ranges.\n\
                     Use context_query(handle=\"{}\", prompt=\"...\") for semantic analysis.",
                    meta.handle_id,
                    meta.file_count,
                    meta.total_lines,
                    meta.total_bytes,
                    meta.summary,
                    meta.handle_id,
                    meta.handle_id,
                    meta.handle_id,
                );
                ToolOutput::ok(&call.id, output)
            }
            Err(e) => ToolOutput::err(&call.id, format!("context_open failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn make_store() -> Arc<Mutex<ContextStore>> {
        Arc::new(Mutex::new(ContextStore::new()))
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "context_open".into(),
            args,
        }
    }

    #[tokio::test]
    async fn opens_file_and_returns_handle() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "line one").unwrap();
        writeln!(tmp, "line two").unwrap();
        tmp.flush().unwrap();

        let tool = ContextOpenTool::new(make_store());
        let out = tool
            .execute(&call(json!({"path": tmp.path().to_str().unwrap()})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("handle=ctx_"), "{}", out.content);
        assert!(out.content.contains("Lines: 2"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_path_returns_error() {
        let tool = ContextOpenTool::new(make_store());
        let out = tool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn nonexistent_path_returns_error() {
        let tool = ContextOpenTool::new(make_store());
        let out = tool
            .execute(&call(json!({"path": "/tmp/sven_no_such_xyz_99"})))
            .await;
        assert!(out.is_error, "{}", out.content);
    }
}
