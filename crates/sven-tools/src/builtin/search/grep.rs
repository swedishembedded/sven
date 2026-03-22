// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use crate::params::{opt_bool, opt_str, opt_u64, require_str};
use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolDisplay, ToolOutput};

/// Cached availability of `rg` (ripgrep).  Probed once on first use; the
/// result never changes during a sven session.
static HAS_RG: OnceLock<bool> = OnceLock::new();

/// Returns `true` if `rg` (ripgrep) is available on `$PATH`.
///
/// Probes by running `rg --version` once; the result is cached for the lifetime
/// of the process.  Does not use `which`/`where` so this works on all platforms.
async fn has_rg() -> bool {
    if let Some(&cached) = HAS_RG.get() {
        return cached;
    }
    let available = tokio::process::Command::new("rg")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);
    // `set` may lose the race; the winner's value is always the same.
    let _ = HAS_RG.set(available);
    available
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Pattern search built on ripgrep.\n\
         pattern: full regex (escape literal braces: \\{\\}).\n\
         include: glob filter (*.rs, **/*.{ts,tsx}).\n\
         whole_project: true → auto-exclude .git/ target/ node_modules/ dist/ __pycache__/ *.lock\n\
         case_sensitive: default true. limit: 100. context_lines: 0.\n\
         output_mode: content (default, file:line:col:text) | files_with_matches | count\n\
         Use files_with_matches for discovery, then read_file for details."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regular expression pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search in (default: current directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob pattern to filter files, e.g. '*.rs' or '*.{ts,tsx}'"
                },
                "whole_project": {
                    "type": "boolean",
                    "description": "Exclude build artifacts (.git, target, node_modules, dist, __pycache__, *.lock). Default false."
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default true)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 100)"
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_with_matches", "count"],
                    "description": "Output format: content (default), files_with_matches, or count"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Lines of context before and after each match (default 0)"
                }
            },
            "required": ["pattern", "path", "include", "case_sensitive", "limit", "output_mode", "context_lines"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let pattern = match require_str(call, "pattern") {
            Ok(p) => p.to_string(),
            Err(e) => return e,
        };
        let path = opt_str(call, "path").unwrap_or(".").to_string();
        let include = opt_str(call, "include").map(str::to_string);
        let whole_project = opt_bool(call, "whole_project").unwrap_or(false);
        let case_sensitive = opt_bool(call, "case_sensitive").unwrap_or(true);
        let limit = opt_u64(call, "limit").unwrap_or(100) as usize;
        let output_mode = opt_str(call, "output_mode").unwrap_or("content");
        let context_lines = opt_u64(call, "context_lines").unwrap_or(0) as usize;

        debug!(pattern = %pattern, path = %path, output_mode = %output_mode, whole_project, "grep tool");

        let result = run_rg(
            &pattern,
            &path,
            include.as_deref(),
            whole_project,
            case_sensitive,
            limit,
            output_mode,
            context_lines,
        )
        .await;

        match result {
            Ok(output) if output.trim().is_empty() => ToolOutput::ok(&call.id, "(no matches)"),
            Ok(output) => ToolOutput::ok(&call.id, output),
            Err(e) => ToolOutput::err(&call.id, format!("grep error: {e}")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_rg(
    pattern: &str,
    path: &str,
    include: Option<&str>,
    whole_project: bool,
    case_sensitive: bool,
    limit: usize,
    output_mode: &str,
    context_lines: usize,
) -> anyhow::Result<String> {
    let output = if has_rg().await {
        let mut args = vec!["--color".to_string(), "never".to_string()];

        match output_mode {
            "files_with_matches" => {
                args.push("-l".to_string());
            }
            "count" => {
                args.push("-c".to_string());
            }
            _ => {
                // content mode: vimgrep format for unambiguous file:line:col:text output
                args.push("--vimgrep".to_string());
                args.push("--no-heading".to_string());
            }
        }

        if !case_sensitive {
            args.push("--ignore-case".to_string());
        }
        if context_lines > 0 && output_mode == "content" {
            args.push(format!("-C{}", context_lines));
        }
        if whole_project {
            for excl in &[
                ".git/**",
                "target/**",
                "node_modules/**",
                "dist/**",
                "__pycache__/**",
                "*.lock",
            ] {
                args.push("--glob".to_string());
                args.push(format!("!{excl}"));
            }
        }
        if let Some(glob) = include {
            args.push("-g".to_string());
            args.push(glob.to_string());
        }
        args.push(pattern.to_string());
        args.push(path.to_string());

        tokio::process::Command::new("rg")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .await?
    } else {
        // Fallback to the system grep (Unix/macOS).  On Windows, grep is not
        // available by default; users should install ripgrep (`winget install BurntSushi.ripgrep`).
        #[cfg(not(windows))]
        {
            let mut args = vec!["-ran".to_string()];
            match output_mode {
                "files_with_matches" => {
                    args.push("-l".to_string());
                }
                "count" => {
                    args.push("-c".to_string());
                }
                _ => {}
            }
            if !case_sensitive {
                args.push("-i".to_string());
            }
            if context_lines > 0 && output_mode == "content" {
                args.push(format!("-C{}", context_lines));
            }
            if whole_project {
                for excl in &[".git", "target", "node_modules", "dist", "__pycache__"] {
                    args.push("--exclude-dir".to_string());
                    args.push(excl.to_string());
                }
            }
            if let Some(glob) = include {
                args.push("--include".to_string());
                args.push(glob.to_string());
            }
            args.push(pattern.to_string());
            args.push(path.to_string());

            tokio::process::Command::new("grep")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .output()
                .await?
        }
        #[cfg(windows)]
        {
            // On Windows, grep is not available. Return an empty output so the
            // caller surfaces a "no matches / rg not found" message.
            // Users should install ripgrep: winget install BurntSushi.ripgrep
            let _ = (
                output_mode,
                context_lines,
                whole_project,
                include,
                case_sensitive,
                pattern,
                path,
            );
            return Err(anyhow::anyhow!(
                "ripgrep (rg) is required for search on Windows. \
                 Install it with: winget install BurntSushi.ripgrep"
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().take(limit).collect();
    let mut result = lines.join("\n");
    let total_lines = stdout.lines().count();
    if total_lines > limit {
        result.push_str(&format!(
            "\n...[{} more matches not shown — narrow with path= or include= to see all results]",
            total_lines - limit
        ));
    }
    Ok(result)
}

impl ToolDisplay for GrepTool {
    fn display_name(&self) -> &str {
        "Grep"
    }
    fn icon(&self) -> &str {
        "🔍"
    }
    fn category(&self) -> &str {
        "search"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        crate::tool_summary::tool_smart_summary("grep", args)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "g1".into(),
            name: "grep".into(),
            args,
        }
    }

    #[tokio::test]
    async fn finds_pattern_in_file() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tool.rs");
        let out = GrepTool
            .execute(&call(json!({
                "pattern": "pub struct",
                "path": path,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("ToolCall") || out.content.contains("ToolOutput"));
    }

    #[tokio::test]
    async fn no_match_returns_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let out = GrepTool
            .execute(&call(json!({
                "pattern": "xyzzy_nonexistent_pattern_12345",
                "path": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let out = GrepTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'pattern'"));
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_grep_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, "Hello World\n").unwrap();

        let out = GrepTool
            .execute(&call(json!({
                "pattern": "hello",
                "path": path,
                "case_sensitive": false
            })))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("Hello"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn limit_truncates_results() {
        // Search in a directory with many matches, limit to 2
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/builtin");
        let out = GrepTool
            .execute(&call(json!({
                "pattern": "pub",
                "path": dir,
                "limit": 2
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Should show truncation notice
        assert!(
            out.content.contains("more") || out.content.lines().count() <= 4,
            "expected truncation or small result set: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn nonexistent_path_returns_no_matches_or_error() {
        let out = GrepTool
            .execute(&call(json!({
                "pattern": "anything",
                "path": "/tmp/sven_no_such_dir_xyzzy_12345"
            })))
            .await;
        // Either an error or a "no matches" result is acceptable
        assert!(
            out.is_error || out.content.contains("no matches") || out.content.contains("error"),
            "unexpected output: {}",
            out.content
        );
    }
}
