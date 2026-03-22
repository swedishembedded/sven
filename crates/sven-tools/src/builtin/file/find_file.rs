// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;
use walkdir::WalkDir;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct FindFileTool;

// Directories that are always excluded from search results.
const EXCLUDED_DIRS: &[&str] = &[".git", "target", "node_modules", ".cargo"];

/// Match a file path (relative to root, using `/` separators) against a glob
/// pattern.  Supports `*` (any chars within a segment), `**` (any segments),
/// and `?` (any single char).  Matching is done on the full relative path so
/// patterns like `**/sven-team/**/*.rs` work correctly.
fn glob_matches(pattern: &str, path: &str, case_insensitive: bool) -> bool {
    // Normalise separators to `/` so patterns work on all platforms.
    let path_norm = path.replace(std::path::MAIN_SEPARATOR, "/");
    let path_str = if case_insensitive {
        path_norm.to_lowercase()
    } else {
        path_norm
    };
    let pat_str = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.to_string()
    };
    glob_match_impl(&pat_str, &path_str)
}

/// Recursive glob matching with `**` support.
fn glob_match_impl(pattern: &str, text: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let txt: Vec<&str> = text.split('/').collect();
    glob_match_segments(&pat, &txt)
}

fn glob_match_segments(pat: &[&str], txt: &[&str]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }
    match pat[0] {
        "**" => {
            // ** can consume zero or more path segments.
            // Try consuming 0 segments first, then 1, 2, …
            if glob_match_segments(&pat[1..], txt) {
                return true;
            }
            if !txt.is_empty() {
                return glob_match_segments(pat, &txt[1..]);
            }
            false
        }
        seg => {
            if txt.is_empty() {
                return false;
            }
            glob_segment_match(seg, txt[0]) && glob_match_segments(&pat[1..], &txt[1..])
        }
    }
}

/// Match a single path segment (no `/` allowed) against a glob pattern using
/// `*` and `?` wildcards.
fn glob_segment_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // dp[i][j] = pattern[..i] matches text[..j]
    let mut dp = vec![vec![false; t.len() + 1]; p.len() + 1];
    dp[0][0] = true;
    // A pattern made only of `*`s matches an empty string.
    for i in 1..=p.len() {
        if p[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }
    for i in 1..=p.len() {
        for j in 1..=t.len() {
            if p[i - 1] == '*' {
                // '*' matches empty or any number of chars.
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if p[i - 1] == '?' || p[i - 1] == t[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }
    dp[p.len()][t.len()]
}

/// Walk `root` recursively and return paths matching `pattern`, up to `max`
/// results.  Skips excluded directories.  Times out at `deadline`.
///
/// Pattern matching rules:
/// - Patterns without `/` (e.g. `*.rs`, `*lint*`) match the **filename** only,
///   matching anywhere in the tree — equivalent to `find -name`.
/// - Patterns with `/` (e.g. `**/*.rs`, `src/**/*.rs`, `**/sven-team/**`)
///   match against the full relative path from `root`.
fn find_files_walkdir(
    root: &str,
    pattern: &str,
    case_insensitive: bool,
    max: usize,
    deadline: std::time::Instant,
) -> anyhow::Result<Vec<String>> {
    let has_path_sep = pattern.contains('/');

    let mut results = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();

    for entry in walker.filter_entry(|e| {
        // Prune excluded directories to avoid traversing them.
        if e.file_type().is_dir() {
            if let Some(name) = e.file_name().to_str() {
                return !EXCLUDED_DIRS.contains(&name);
            }
        }
        true
    }) {
        // Check the timeout on each entry to keep latency bounded.
        if std::time::Instant::now() > deadline {
            break;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        // Compute a relative path from root using `/` separators.
        let rel_path = match entry.path().strip_prefix(root) {
            Ok(p) => p.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"),
            Err(_) => entry.path().to_string_lossy().to_string(),
        };
        // Remove a leading `./` that walkdir sometimes adds.
        let rel_path = rel_path.trim_start_matches("./");

        // Choose what to match against based on whether the pattern contains `/`:
        // - No `/`:  match filename only (classic `find -name` semantics)
        // - With `/`: match full relative path (supports `**/sven-team/**/*.rs`)
        let match_target = if has_path_sep {
            rel_path
        } else {
            rel_path.split('/').next_back().unwrap_or(rel_path)
        };

        if glob_matches(pattern, match_target, case_insensitive) {
            results.push(entry.path().display().to_string());
            if results.len() >= max {
                break;
            }
        }
    }

    Ok(results)
}

#[async_trait]
impl Tool for FindFileTool {
    fn name(&self) -> &str {
        "find_file"
    }

    fn description(&self) -> &str {
        "Find files by name glob pattern, searching recursively under a root directory.\n\
         Pure-Rust implementation (walkdir); excludes .git/, target/, node_modules/, .cargo/registry/.\n\
         Glob patterns:\n\
           '*.rs'               — all .rs files anywhere under root\n\
           '**/*.rs'            — same (**/ prefix is stripped; search is always recursive)\n\
           'src/**/*.rs'        — .rs files under <root>/src/\n\
           '**/sven-team/**'    — all files inside any directory named 'sven-team'\n\
           '**/sven-team/**/*.rs' — .rs files inside any 'sven-team' directory\n\
           'Cargo.toml'         — exact filename anywhere under root\n\
           '*lint*'             — filenames containing 'lint'\n\
         For content search use grep or search_codebase instead."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Filename glob pattern. Examples: '*.rs', '**/*.toml', 'src/**/*.c', '*lint*', 'Cargo.toml'"
                },
                "root": {
                    "type": "string",
                    "description": "Root directory to search from (default: current directory)"
                },
                "case_insensitive": {
                    "type": "boolean",
                    "description": "Match filenames case-insensitively (default: false)"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 200)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Hard timeout in seconds (default: 10)"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let raw_pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'pattern'"),
        };
        let root = call
            .args
            .get("root")
            .and_then(|v| v.as_str())
            .unwrap_or(".")
            .to_string();
        let case_insensitive = call
            .args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = call
            .args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;
        let timeout_secs = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        debug!(pattern = %raw_pattern, root = %root, "find_file tool");

        let pattern = raw_pattern.clone();
        let root_path = root.clone();

        // Run the walkdir traversal on a blocking thread to avoid blocking
        // the async executor.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        let result = tokio::task::spawn_blocking(move || {
            find_files_walkdir(&root_path, &pattern, case_insensitive, max, deadline)
        })
        .await;

        match result {
            Ok(Ok(matches)) => {
                if matches.is_empty() {
                    ToolOutput::ok(&call.id, "(no matches)")
                } else {
                    ToolOutput::ok(&call.id, matches.join("\n"))
                }
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("find_file error: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("find_file task error: {e}")),
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
            id: "f1".into(),
            name: "find_file".into(),
            args,
        }
    }

    // ── Glob matching ─────────────────────────────────────────────────────────

    #[test]
    fn glob_matches_simple_pattern() {
        assert!(glob_matches("*.rs", "foo.rs", false));
        assert!(!glob_matches("*.rs", "foo.toml", false));
    }

    #[test]
    fn glob_matches_case_insensitive() {
        assert!(glob_matches("*.md", "README.MD", true));
        assert!(!glob_matches("*.md", "README.MD", false));
    }

    #[test]
    fn glob_matches_double_star() {
        // Patterns without `/` match filename only (find -name semantics).
        // The caller is responsible for passing the filename when has_path_sep=false.
        assert!(glob_matches("*.rs", "lib.rs", false));
        // With `/`, match the full path.
        assert!(glob_matches("**/*.rs", "src/lib.rs", false));
        assert!(glob_matches(
            "**/*.rs",
            "crates/sven-team/src/lib.rs",
            false
        ));
    }

    #[test]
    fn glob_matches_dir_pattern() {
        // Pattern with `/`: matches against full relative path.
        assert!(glob_matches("sven-team/**", "sven-team/src/lib.rs", false));
        assert!(!glob_matches("sven-team/**", "other/src/lib.rs", false));
    }

    #[test]
    fn glob_matches_dir_anywhere() {
        // Pattern with ** on both sides matches inside any directory.
        assert!(glob_matches(
            "**/sven-team/**",
            "crates/sven-team/src/lib.rs",
            false
        ));
        assert!(!glob_matches(
            "**/sven-team/**",
            "crates/other/src/lib.rs",
            false
        ));
    }

    // ── Search execution ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn finds_toml_files() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.toml",
                "root": crate_root,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_double_star_pattern() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/*.toml",
                "root": crate_root,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_subdirectory_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("lib");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("main.rs"), b"fn main() {}").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "src/**/*.rs",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("main.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_in_nested_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("build").join("zephyr");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("zephyr.elf"), b"\x7fELF").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.elf",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("zephyr.elf"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("README.MD"), b"docs").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.md",
                "root": dir.path().to_str().unwrap(),
                "case_insensitive": true
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("README.MD"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_with_wildcard_name_pattern() {
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*lint*",
                "root": src,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("read_lints"), "{}", out.content);
    }

    #[tokio::test]
    async fn max_results_is_respected() {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.rs",
                "root": crate_root,
                "max_results": 3
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let lines: Vec<&str> = out.content.lines().collect();
        assert!(lines.len() <= 3, "expected ≤3 results, got {}", lines.len());
    }

    #[tokio::test]
    async fn no_match_returns_no_matches_message() {
        let dir = tempfile::tempdir().unwrap();
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*.xyz_nonexistent_ext",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_pattern_is_error() {
        let out = FindFileTool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'pattern'"), "{}", out.content);
    }

    #[test]
    fn schema_requires_only_pattern() {
        let schema = FindFileTool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert!(required.iter().any(|v| v.as_str() == Some("pattern")));
    }

    // ── Execute with path-glob patterns ──────────────────────────────────────

    #[tokio::test]
    async fn finds_files_under_dir_anywhere_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        // Create nested: root/crates/sven-team/src/lib.rs
        let sub = dir.path().join("crates").join("sven-team").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), b"pub fn foo() {}").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/sven-team/**",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("lib.rs"), "{}", out.content);
    }

    #[tokio::test]
    async fn finds_rs_files_under_dir_anywhere_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("crates").join("sven-team").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("lib.rs"), b"pub fn foo() {}").unwrap();
        std::fs::write(sub.join("README.md"), b"# readme").unwrap();

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "**/sven-team/**/*.rs",
                "root": dir.path().to_str().unwrap()
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("lib.rs"), "{}", out.content);
        assert!(!out.content.contains("README.md"), "{}", out.content);
    }
}
