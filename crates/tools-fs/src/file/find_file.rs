// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;
use walkdir::WalkDir;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};

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
            // Try consuming 0 segments first, then 1, 2, ...
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

/// How many paths an untuned `find_file` returns.
///
/// A tool has to be useful before anyone has tuned it, and the first call is
/// always the untuned one. 200 absolute paths against a deep tree measured
/// 19 KB - enough to end a turn on a 40k context - so the default is a page
/// the caller can act on, with the count of what was left behind.
pub(crate) const DEFAULT_MAX_RESULTS: usize = 40;

/// Hard ceiling on the reply, whatever `max_results` says.
///
/// `max_results` bounds the number of paths, not their length, and one deeply
/// nested tree defeats any count-based budget on its own.
pub(crate) const MAX_OUTPUT_CHARS: usize = 4000;

/// Walk `root` recursively and return paths matching `pattern`, up to `max`
/// results.  Skips excluded directories.  Times out at `deadline`.
///
/// Pattern matching rules:
/// - Patterns without `/` (e.g. `*.rs`, `*lint*`) match the **filename** only,
///   matching anywhere in the tree - equivalent to `find -name`.
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
            // The relative path, not the absolute one: `root` is an input the
            // caller already has, and repeating it per line is the longest
            // part of every line.
            results.push(rel_path.to_string());
            // One past the budget, so the caller can distinguish "exactly max
            // matches" from "more than max" without a second traversal.
            if results.len() > max {
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
        "Find files by filename glob, recursively, under a root directory.\n\
         Returns paths relative to 'root', 40 at a time by default; the reply says when\n\
         more matched, and 'max_results' raises the cap.\n\
         'root' defaults to the working directory, so an unset root lists from where the\n\
         session is, not from the filesystem root.\n\
         Skips .git/, target/, node_modules/, .cargo/registry/.\n\
         Patterns: '*.rs' matches that filename anywhere below root; a pattern containing\n\
         '/' matches the whole relative path, so 'src/**/*.rs' and '**/parser/**' work.\n\
         Matches on names only - it never opens a file."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Filename glob. Omit to list everything. Examples: '*.rs', 'src/**/*.c', '*lint*'"
                },
                "root": {
                    "type": "string",
                    "description": "Directory to search from. Omit for the working directory."
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
            "required": [],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        // An argument sent as "" means the caller did not supply one. Models
        // routinely fill in every field of a schema rather than omitting the
        // optional ones, and taking that literally searched nowhere for
        // nothing - then reported success, which reads as "the directory is
        // empty" rather than "you called me wrong".
        let supplied = |key: &str| -> Option<String> {
            call.args
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        // No pattern means every file, which is the plain reading of "what is
        // in here" and is safe to answer now that the reply is a bounded page.
        let raw_pattern = supplied("pattern").unwrap_or_else(|| "*".to_string());
        let root = supplied("root").unwrap_or_else(|| ".".to_string());
        let case_insensitive = call
            .args
            .get("case_insensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let max = call
            .args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_MAX_RESULTS as u64) as usize;
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
                    ToolOutput::ok(&call.id, render_page(matches, max))
                }
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("find_file error: {e}")),
            Err(e) => ToolOutput::err(&call.id, format!("find_file task error: {e}")),
        }
    }
}

/// Render at most `max` paths, within [`MAX_OUTPUT_CHARS`], saying plainly
/// when something was left out and how to ask for it.
///
/// `matches` may hold one extra entry past `max` - that is how the walker
/// signals "there were more" - so the surplus is dropped here rather than
/// shown.
fn render_page(mut matches: Vec<String>, max: usize) -> String {
    let overflowed = matches.len() > max;
    matches.truncate(max);

    let mut out = String::new();
    let mut shown = 0usize;
    for path in &matches {
        // Leave room for the notice that has to follow a cut.
        if out.len() + path.len() + 1 > MAX_OUTPUT_CHARS.saturating_sub(120) {
            break;
        }
        out.push_str(path);
        out.push('\n');
        shown += 1;
    }

    let held_back = matches.len() - shown;
    if overflowed || held_back > 0 {
        // Naming both ways forward matters: raising the cap is right when the
        // set really is the answer, narrowing the pattern when it is not, and
        // only the caller knows which.
        let more = if overflowed {
            format!("more than {max}")
        } else {
            format!("{held_back} more")
        };
        out.push_str(&format!(
            "[{shown} shown, {more} matched - narrow 'pattern' or raise 'max_results']\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

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
        assert!(glob_matches("**/*.rs", "crates/team/src/lib.rs", false));
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
        assert!(glob_matches("**/team/**", "crates/team/src/lib.rs", false));
        assert!(!glob_matches(
            "**/team/**",
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
        // Fixture assumption: this crate's own src/ tree contains a file
        // matching the pattern. Was `*lint*` / "read_lints" back when this
        // test lived in sven-tools's builtin/file/ alongside builtin/system/
        // read_lints.rs; that file stayed behind in sven-tools when this test
        // moved to sven-tools-fs (5.2 of the refactor plan's god-crate
        // splits), so the pattern now targets a file this crate does own.
        let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*read_lint*",
                "root": src,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("no matches"), "{}", out.content);

        let out = FindFileTool
            .execute(&call(json!({
                "pattern": "*read_file*",
                "root": src,
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("read_file"), "{}", out.content);
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
        // Paths, not lines: a truncated page also carries a notice line, and
        // counting that as a result is what made this assertion ambiguous.
        let paths = out.content.lines().filter(|l| l.ends_with(".rs")).count();
        assert!(paths <= 3, "expected <=3 results, got {paths}");
        assert!(
            out.content.contains("max_results"),
            "a cut page must say so: {}",
            out.content
        );
    }

    /// An argument sent as "" means the caller did not supply one.
    ///
    /// A model that fills in every field of a schema rather than omitting the
    /// optional ones is not doing anything wrong, and it is common: the empty
    /// string arrives as a value, misses `unwrap_or`, and was taken literally -
    /// an empty pattern matched nothing and an empty root searched nowhere.
    /// The reply was "(no matches)" with success=true, so "list the files
    /// here" came back as a confident claim that the directory was empty.
    #[tokio::test]
    async fn empty_arguments_mean_unspecified_not_literal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("only.txt"), "x").unwrap();

        let out = FindFileTool
            .execute(&call(
                json!({"pattern": "", "root": dir.path().to_str().unwrap()}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("only.txt"),
            "an empty pattern must list everything, not nothing: {}",
            out.content
        );
    }

    /// An omitted or empty root searches the working directory.
    ///
    /// Asserted against the crate directory - which is where cargo runs these
    /// - rather than by moving the process into a tempdir: the working
    /// directory is process-wide state, and these tests run in parallel.
    #[tokio::test]
    async fn an_empty_root_is_the_working_directory() {
        let out = FindFileTool
            .execute(&call(json!({"pattern": "Cargo.toml", "root": ""})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    /// A search nobody bounded must still come back small.
    ///
    /// The default used to be 200 absolute paths joined with newlines and no
    /// byte budget at all: a `*` against a deep tree produced 19 KB, which
    /// overflowed the model's context and ended the turn. A tool has to be
    /// usable without being tuned first, so the untuned call returns a page.
    #[tokio::test]
    async fn an_unbounded_search_returns_one_small_page() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(DEFAULT_MAX_RESULTS * 3) {
            std::fs::write(dir.path().join(format!("file_{i:04}.txt")), "x").unwrap();
        }

        let out = FindFileTool
            .execute(&call(
                json!({"pattern": "*", "root": dir.path().to_str().unwrap()}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);

        let listed = out.content.lines().filter(|l| l.ends_with(".txt")).count();
        assert!(
            listed <= DEFAULT_MAX_RESULTS,
            "listed {listed} paths, budget is {DEFAULT_MAX_RESULTS}"
        );
        assert!(
            out.content.len() <= MAX_OUTPUT_CHARS,
            "reply was {} chars, budget is {MAX_OUTPUT_CHARS}",
            out.content.len()
        );
        // Truncating silently would leave the caller believing it saw
        // everything, so the reply has to say otherwise and how to go on.
        assert!(
            out.content.contains("more"),
            "no indication results were cut: {}",
            out.content
        );
        assert!(
            out.content.contains("max_results"),
            "no way offered to see more: {}",
            out.content
        );
    }

    /// Paths are reported relative to the root that was searched.
    ///
    /// The root is an input the caller already has, so repeating it on every
    /// line buys nothing and costs the deepest part of each path. Measured at
    /// roughly 95 characters per line against a real tree.
    #[tokio::test]
    async fn results_are_relative_to_the_search_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/deep.rs"), "x").unwrap();

        let out = FindFileTool
            .execute(&call(
                json!({"pattern": "*.rs", "root": dir.path().to_str().unwrap()}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("a/b/deep.rs"), "{}", out.content);
        assert!(
            !out.content.contains(dir.path().to_str().unwrap()),
            "the root is repeated on every line: {}",
            out.content
        );
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
    async fn a_call_with_no_arguments_lists_the_working_directory() {
        // Refusing a bare call would be the tool getting in the way: "what is
        // in here" is a complete question, and the answer is a bounded page.
        let out = FindFileTool.execute(&call(json!({}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Cargo.toml"), "{}", out.content);
    }

    #[test]
    fn schema_requires_nothing() {
        let schema = FindFileTool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(
            required.is_empty(),
            "every argument has a usable default: {required:?}"
        );
    }

    // ── Execute with path-glob patterns ──────────────────────────────────────

    #[tokio::test]
    async fn finds_files_under_dir_anywhere_in_tree() {
        let dir = tempfile::tempdir().unwrap();
        // Create nested: root/crates/team/src/lib.rs
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
