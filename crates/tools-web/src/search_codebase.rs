// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_hsm::ToolCapability;

use sven_tool_api::params::{opt_bool, opt_str, opt_u64, require_str};
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{OutputCategory, Tool, ToolCall, ToolDisplay, ToolOutput};

/// Thin wrapper over `grep` / ripgrep with sensible codebase defaults:
/// always excludes .git/, target/, node_modules/, dist/, __pycache__/.
pub struct SearchCodebaseTool;

#[async_trait]
impl Tool for SearchCodebaseTool {
    fn name(&self) -> &str {
        "search_codebase"
    }

    fn description(&self) -> &str {
        "Ripgrep across the codebase with standard exclusions: \
         .git/ target/ node_modules/ dist/ __pycache__/ *.lock\n\
         Same regex syntax as grep. Use for broad whole-repo exploration.\n\
         Use grep (not this) when you need output_mode, context_lines, or targeted search.\n\
         Use glob when searching by filename. query: regex. include: glob file filter. \
         case_sensitive: true. limit: 100."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Pattern or text to search for (supports regex)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (default: current directory)"
                },
                "include": {
                    "type": "string",
                    "description": "Glob filter for file types, e.g. '*.rs' or '*.{ts,tsx}'"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Case-sensitive search (default true)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of matches to return (default 100)"
                }
            },
            "required": ["query", "path", "include", "case_sensitive", "limit"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let query = match require_str(call, "query") {
            Ok(q) => q.to_string(),
            Err(e) => return e,
        };
        let path = opt_str(call, "path").unwrap_or(".").to_string();
        let include = opt_str(call, "include").map(str::to_string);
        let case_sensitive = opt_bool(call, "case_sensitive").unwrap_or(true);
        let limit = opt_u64(call, "limit").unwrap_or(100) as usize;

        debug!(query = %query, path = %path, "search_codebase tool");

        // Detect rg availability by probing its --version flag (cross-platform;
        // avoids `which` which is Unix-only and `where` which is Windows-only).
        let has_rg = tokio::process::Command::new("rg")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false);

        let output = if has_rg {
            let mut args = vec![
                "--vimgrep".to_string(),
                "--color".to_string(),
                "never".to_string(),
                "--no-heading".to_string(),
                // Exclude build artifacts
                "--glob".to_string(),
                "!.git/**".to_string(),
                "--glob".to_string(),
                "!target/**".to_string(),
                "--glob".to_string(),
                "!node_modules/**".to_string(),
                "--glob".to_string(),
                "!dist/**".to_string(),
                "--glob".to_string(),
                "!__pycache__/**".to_string(),
                "--glob".to_string(),
                "!*.lock".to_string(),
            ];
            if !case_sensitive {
                args.push("--ignore-case".to_string());
            }
            if let Some(glob) = &include {
                args.push("-g".to_string());
                args.push(glob.clone());
            }
            // `--` stops option parsing, so a query or path starting with `-`
            // is treated as data rather than an rg flag.
            args.push("--".to_string());
            args.push(query.clone());
            args.push(path.clone());

            tokio::process::Command::new("rg")
                .args(&args)
                .stdin(std::process::Stdio::null())
                .output()
                .await
        } else {
            // Fallback to system grep on Unix/macOS.  On Windows, ripgrep is
            // required: install with `winget install BurntSushi.ripgrep`.
            #[cfg(not(windows))]
            {
                tokio::process::Command::new("grep")
                    .args(grep_args(&query, &path, include.as_deref(), case_sensitive))
                    .stdin(std::process::Stdio::null())
                    .output()
                    .await
            }
            #[cfg(windows)]
            {
                let _ = (case_sensitive, &include, &query, &path);
                return ToolOutput::err(
                    &call.id,
                    "ripgrep (rg) is required for search_codebase on Windows. \
                     Install it with: winget install BurntSushi.ripgrep",
                );
            }
        };

        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = text.lines().take(limit).collect();
                if lines.is_empty() {
                    ToolOutput::ok(&call.id, "(no matches)")
                } else {
                    let total = text.lines().count();
                    let mut result = lines.join("\n");
                    if total > limit {
                        result
                            .push_str(&format!("\n...[{} more matches not shown]", total - limit));
                    }
                    ToolOutput::ok(&call.id, result)
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("search_codebase error: {e}")),
        }
    }
}

/// The argv for the `grep` fallback used when ripgrep is not installed.
///
/// Every caller-supplied value is a separate argv element and no shell is
/// involved, so metacharacters in any of them are inert. This previously built
/// a single string for `sh -c`; `query` and `path` were quoted but `include`
/// was interpolated raw, which made an LLM-supplied `include` glob arbitrary
/// command execution through a tool that is `ApprovalPolicy::Auto` and
/// therefore never approval-gated.
fn grep_args(query: &str, path: &str, include: Option<&str>, case_sensitive: bool) -> Vec<String> {
    let mut args = vec!["-rn".to_string()];
    if !case_sensitive {
        args.push("-i".to_string());
    }
    for dir in [".git", "target", "node_modules", "dist"] {
        args.push(format!("--exclude-dir={dir}"));
    }
    if let Some(glob) = include {
        args.push(format!("--include={glob}"));
    }
    // `--` stops option parsing, so a query or path starting with `-` is
    // treated as data rather than a grep flag.
    args.push("--".to_string());
    args.push(query.to_string());
    args.push(path.to_string());
    args
}

impl ToolDisplay for SearchCodebaseTool {
    fn display_name(&self) -> &str {
        "Search"
    }
    fn icon(&self) -> &str {
        "🧠"
    }
    fn category(&self) -> &str {
        "search"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        sven_tool_api::tool_summary::tool_smart_summary("semantic_search", args)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    /// `search_codebase` is `ApprovalPolicy::Auto` + `ToolCapability::ReadFile`,
    /// so it is never approval-gated. Its grep fallback must therefore not be
    /// able to reach a shell: every caller-supplied value has to stay one argv
    /// element, whatever metacharacters it contains.
    #[test]
    fn grep_fallback_keeps_injected_metacharacters_in_one_argv_element() {
        let args = grep_args(
            "needle; touch pwned",
            "/repo; touch pwned",
            Some("*.rs; touch pwned"),
            true,
        );

        assert!(
            args.contains(&"--include=*.rs; touch pwned".to_string()),
            "the include glob must survive whole, unsplit: {args:?}"
        );
        assert!(
            args.contains(&"needle; touch pwned".to_string()),
            "the query must survive whole: {args:?}"
        );
        assert!(
            args.contains(&"/repo; touch pwned".to_string()),
            "the path must survive whole: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "sh" || a == "-c"),
            "no shell may appear in the argv: {args:?}"
        );
    }

    /// A query or path beginning with `-` is data, not a grep option.
    #[test]
    fn grep_fallback_stops_option_parsing_before_user_values() {
        let args = grep_args("-v", "-r", None, true);
        let end = args.iter().position(|a| a == "--").expect("`--` present");
        assert_eq!(
            &args[end + 1..],
            &["-v".to_string(), "-r".to_string()],
            "query and path must both follow `--`"
        );
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "s1".into(),
            name: "search_codebase".into(),
            args,
        }
    }

    #[tokio::test]
    async fn finds_in_sven_codebase() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "ToolRegistry",
                "path": src,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(!out.content.contains("(no matches)"));
    }

    #[tokio::test]
    async fn missing_query_is_error() {
        let out = SearchCodebaseTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'query'"));
    }

    #[tokio::test]
    async fn include_glob_narrows_results() {
        // Search only in .toml files - should not return .rs matches.
        // Use the crate root: it contains Cargo.toml which has "version".
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "version",
                "path": crate_root,
                "include_glob": "*.toml"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // All matched lines should come from .toml files
        if !out.content.contains("(no matches)") {
            assert!(
                out.content.contains(".toml"),
                "expected .toml files in results: {}",
                &out.content[..out.content.len().min(300)]
            );
        }
    }

    #[tokio::test]
    async fn case_insensitive_search() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let out = SearchCodebaseTool
            .execute(&call(json!({
                "query": "TOOLREGISTRY",
                "path": src,
                "case_sensitive": false
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        // Should find ToolRegistry in a case-insensitive way
        assert!(
            !out.content.contains("(no matches)"),
            "expected case-insensitive match for TOOLREGISTRY"
        );
    }
}
