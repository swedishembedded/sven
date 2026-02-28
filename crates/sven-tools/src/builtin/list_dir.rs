// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct ListDirTool;

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List directory contents. depth: default 2, max 5. limit: 100 entries by default.\n\
         Excludes .git/ target/ node_modules/. Directories have trailing /.\n\
         For file pattern search use glob; for content search use grep."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the directory"
                },
                "depth": {
                    "type": "integer",
                    "description": "Maximum recursion depth (default 2, max 5)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of entries to return (default 100)"
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
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
        let depth = call
            .args
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(2)
            .min(5) as usize;
        let limit = call
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        debug!(path = %path, depth, limit, "list_dir tool");

        // Fail early if the path doesn't exist or isn't a directory
        match tokio::fs::metadata(&path).await {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return ToolOutput::err(&call.id, format!("not a directory: {path}")),
            Err(e) => return ToolOutput::err(&call.id, format!("cannot access {path}: {e}")),
        }

        let mut entries: Vec<String> = Vec::new();
        let mut truncated = false;

        collect_entries(&path, &path, 0, depth, limit, &mut entries, &mut truncated).await;

        if entries.is_empty() {
            return ToolOutput::ok(&call.id, "(empty directory)");
        }

        let mut output = entries.join("\n");
        if truncated {
            output.push_str(&format!("\n...[output truncated at {} entries]", limit));
        }

        ToolOutput::ok(&call.id, output)
    }
}

static EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".svn",
    "__pycache__",
    ".mypy_cache",
];

fn is_excluded(name: &str) -> bool {
    EXCLUDED_DIRS.contains(&name)
}

fn relative_path(base: &str, full: &str) -> String {
    if let Some(stripped) = full.strip_prefix(base) {
        stripped.trim_start_matches('/').to_string()
    } else {
        full.to_string()
    }
}

#[async_recursion::async_recursion]
async fn collect_entries(
    base: &str,
    dir: &str,
    current_depth: usize,
    max_depth: usize,
    limit: usize,
    entries: &mut Vec<String>,
    truncated: &mut bool,
) {
    if entries.len() >= limit {
        *truncated = true;
        return;
    }

    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(_) => return,
    };

    let mut children: Vec<(String, bool)> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
        children.push((name, is_dir));
    }
    children.sort_by(|(a, a_dir), (b, b_dir)| {
        // Directories first, then alphabetical
        b_dir.cmp(a_dir).then(a.cmp(b))
    });

    for (name, is_dir) in children {
        if entries.len() >= limit {
            *truncated = true;
            return;
        }
        let full_path = format!("{}/{}", dir.trim_end_matches('/'), name);
        let rel = relative_path(base, &full_path);
        if is_dir {
            entries.push(format!("{}/", rel));
            if current_depth < max_depth && !is_excluded(&name) {
                collect_entries(
                    base,
                    &full_path,
                    current_depth + 1,
                    max_depth,
                    limit,
                    entries,
                    truncated,
                )
                .await;
            }
        } else {
            entries.push(rel);
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
            id: "l1".into(),
            name: "list_dir".into(),
            args,
        }
    }

    #[tokio::test]
    async fn lists_directory_contents() {
        let t = ListDirTool;
        let out = t.execute(&call(json!({"path": "/tmp"}))).await;
        assert!(!out.is_error, "{}", out.content);
    }

    #[tokio::test]
    async fn dirs_have_trailing_slash() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let dir = format!("/tmp/sven_listdir_{}_{n}", std::process::id());
        std::fs::create_dir_all(format!("{dir}/subdir")).unwrap();
        std::fs::write(format!("{dir}/file.txt"), "x").unwrap();

        let t = ListDirTool;
        let out = t.execute(&call(json!({"path": dir}))).await;
        assert!(
            out.content.contains("subdir/"),
            "dirs should have trailing slash"
        );
        assert!(out.content.contains("file.txt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_dir_path_is_error() {
        let t = ListDirTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'path'"));
    }

    #[tokio::test]
    async fn depth_zero_shows_only_immediate_children() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let dir = format!("/tmp/sven_listdir_depth_{}_{n}", std::process::id());
        std::fs::create_dir_all(format!("{dir}/subdir/nested")).unwrap();
        std::fs::write(format!("{dir}/top.txt"), "x").unwrap();
        std::fs::write(format!("{dir}/subdir/inner.txt"), "x").unwrap();

        let t = ListDirTool;
        // depth=0 means no recursion: only show immediate children
        let out = t.execute(&call(json!({"path": dir, "depth": 0}))).await;
        assert!(out.content.contains("top.txt"));
        assert!(out.content.contains("subdir/"));
        assert!(
            !out.content.contains("inner.txt"),
            "inner.txt should not appear at depth=0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn nonexistent_dir_is_error() {
        let t = ListDirTool;
        let out = t
            .execute(&call(json!({"path": "/tmp/sven_no_such_dir_xyzzy_99999"})))
            .await;
        assert!(out.is_error);
    }
}
