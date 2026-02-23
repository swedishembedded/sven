// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT

use async_trait::async_trait;
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Minimum similarity ratio (0–1) for a fuzzy window to be accepted.
const FUZZY_THRESHOLD: f64 = 0.85;

/// Number of "did you mean?" suggestions shown in the failure message.
const MAX_SUGGESTIONS: usize = 3;

// ── Matching helpers ──────────────────────────────────────────────────────────

/// Strip `L<n>:` prefixes that `read_file` adds to each output line.
/// The model sometimes copies those prefixes verbatim into `old_str`.
///
/// Only strips the prefix when there is at least one digit between `L` and `:`,
/// so lines like `L:foo` (Go/C labels) or `L` alone are left unchanged.
fn strip_read_file_prefixes(s: &str) -> String {
    let trailing_newline = s.ends_with('\n');
    let stripped: Vec<&str> = s
        .lines()
        .map(|line| {
            if let Some(after_l) = line.strip_prefix('L') {
                if let Some(colon) = after_l.find(':') {
                    // Require at least one digit — guards against "L:label" lines.
                    if colon > 0 && after_l[..colon].chars().all(|c| c.is_ascii_digit()) {
                        return &after_l[colon + 1..];
                    }
                }
            }
            line
        })
        .collect();
    let mut out = stripped.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

/// Strip the common leading whitespace from every non-empty line of `s`.
fn strip_common_indent(s: &str) -> String {
    let min_indent = s
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    let lines: Vec<&str> = s
        .lines()
        .map(|l| if l.len() >= min_indent { &l[min_indent..] } else { l.trim_start() })
        .collect();

    let mut out = lines.join("\n");
    if s.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Try to find `needle` in `content` after stripping common indentation from
/// each side independently. Returns the **actual text in the file** for the
/// matching window so the caller can do a direct `content.replacen(actual, …)`.
fn try_indent_normalized(content: &str, needle: &str) -> Option<String> {
    let n = needle.lines().count();
    if n == 0 {
        return None;
    }

    let norm_needle = strip_common_indent(needle);
    let norm_cmp = norm_needle.trim_end_matches('\n');

    let file_lines: Vec<&str> = content.lines().collect();
    if file_lines.len() < n {
        return None;
    }

    for i in 0..=(file_lines.len() - n) {
        let window = file_lines[i..i + n].join("\n");
        let norm_window = strip_common_indent(&window);
        if norm_window.trim_end_matches('\n') == norm_cmp {
            return Some(if needle.ends_with('\n') { window + "\n" } else { window });
        }
    }
    None
}

/// Compute a similarity ratio in [0, 1] between two strings using character-level
/// diff (2 × matching_bytes / total_bytes — same formula as Python difflib).
///
/// Character-level diff is used so that minor single-character differences (e.g.
/// "u32" vs "usize") still yield a high ratio even within single-line content,
/// rather than the 0 that line-level diff would return for any two non-equal lines.
fn similarity_ratio(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let diff = TextDiff::from_chars(a, b);
    let matching: usize = diff
        .iter_all_changes()
        .filter(|c| c.tag() == ChangeTag::Equal)
        .map(|c| c.value().len())
        .sum();
    (matching * 2) as f64 / total as f64
}

/// Slide a window of `old_str.lines().count()` lines over `content` and find
/// the single window whose similarity to `old_str` exceeds `FUZZY_THRESHOLD`.
/// Returns `None` if zero or multiple windows qualify (avoids ambiguous edits).
fn try_fuzzy(content: &str, old_str: &str) -> Option<String> {
    let n = old_str.lines().count().max(1);
    let file_lines: Vec<&str> = content.lines().collect();
    if file_lines.len() < n {
        return None;
    }

    let old_cmp = old_str.trim_end_matches('\n');
    let mut hits: Vec<String> = Vec::new();

    for i in 0..=(file_lines.len() - n) {
        let window = file_lines[i..i + n].join("\n");
        if similarity_ratio(old_cmp, &window) >= FUZZY_THRESHOLD {
            hits.push(if old_str.ends_with('\n') { window + "\n" } else { window });
        }
    }

    // Only accept an unambiguous single match.
    if hits.len() == 1 { hits.pop() } else { None }
}

/// Find the `limit` windows in `content` most similar to `old_str`, for use
/// in "did you mean?" error messages. Returns `(ratio, 1-based line, text)`.
fn find_similar_blocks(content: &str, old_str: &str, limit: usize) -> Vec<(f64, usize, String)> {
    let n = old_str.lines().count().max(1);
    let file_lines: Vec<&str> = content.lines().collect();
    if file_lines.len() < n {
        return vec![];
    }

    let old_cmp = old_str.trim_end_matches('\n');
    let mut candidates: Vec<(f64, usize, String)> = file_lines
        .windows(n)
        .enumerate()
        .map(|(i, win)| {
            let text = win.join("\n");
            let ratio = similarity_ratio(old_cmp, &text);
            (ratio, i + 1, text)
        })
        .filter(|(r, _, _)| *r > 0.3)
        .collect();

    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(limit);
    candidates
}

// ── Tool ──────────────────────────────────────────────────────────────────────

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }

    fn description(&self) -> &str {
        "Replace text in a file. Tries strategies in order until one succeeds:\n\
         1. Exact match\n\
         2. Strip read_file display prefixes ('L<n>:') from old_str, then exact match\n\
         3. Strip prefixes then normalize indentation\n\
         4. Normalize indentation of original old_str\n\
         5. Fuzzy match (≥85% similarity, unique location only)\n\
         On all-fail, shows the most similar sections from the file to help fix old_str.\n\
         \n\
         IMPORTANT: old_str must match FILE content, not read_file display output.\n\
         read_file shows lines as 'L<n>:content' — those 'L<n>:' prefixes are NOT in the file.\n\
         After a successful edit the file has changed; re-read before the next edit.\n\
         replace_all=true: replace every occurrence (e.g. rename a symbol across a file).\n\
         Fuzzy matching is skipped when replace_all=true."
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
                    "description": "Text to find. Matched exactly first; minor prefix/indent differences are corrected automatically."
                },
                "new_str": {
                    "type": "string",
                    "description": "Replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences instead of requiring uniqueness (default false)"
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
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "missing required parameter 'path'. Received: {}",
                        serde_json::to_string(&call.args).unwrap_or_default()
                    ),
                )
            }
        };
        let old_str = match call.args.get("old_str").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "missing required parameter 'old_str'. Received: {}",
                        serde_json::to_string(&call.args).unwrap_or_default()
                    ),
                )
            }
        };
        let new_str = match call.args.get("new_str").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "missing required parameter 'new_str'. Received: {}",
                        serde_json::to_string(&call.args).unwrap_or_default()
                    ),
                )
            }
        };

        if old_str.is_empty() {
            return ToolOutput::err(
                &call.id,
                "old_str must not be empty; provide the text you want to replace".to_string(),
            );
        }

        let replace_all =
            call.args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        debug!(path = %path, replace_all = %replace_all, "edit_file tool");

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(&call.id, format!("read error: {e}")),
        };

        // ── Strategy 1: exact match ──────────────────────────────────────────
        if let Some(out) =
            self.try_apply(&call.id, &path, &content, &old_str, &new_str, replace_all, "exact")
                .await
        {
            return out;
        }

        // ── Strategy 2: strip L<n>: prefixes → exact ────────────────────────
        let stripped = strip_read_file_prefixes(&old_str);
        if stripped != old_str {
            if let Some(out) = self
                .try_apply(
                    &call.id,
                    &path,
                    &content,
                    &stripped,
                    &new_str,
                    replace_all,
                    "strip-prefixes",
                )
                .await
            {
                return out;
            }

            // ── Strategy 3: strip prefixes → indent-normalize ───────────────
            if let Some(actual) = try_indent_normalized(&content, &stripped) {
                if let Some(out) = self
                    .try_apply(
                        &call.id,
                        &path,
                        &content,
                        &actual,
                        &new_str,
                        replace_all,
                        "strip-prefixes+indent",
                    )
                    .await
                {
                    return out;
                }
            }
        }

        // ── Strategy 4: indent-normalize original old_str ───────────────────
        if let Some(actual) = try_indent_normalized(&content, &old_str) {
            if let Some(out) = self
                .try_apply(
                    &call.id,
                    &path,
                    &content,
                    &actual,
                    &new_str,
                    replace_all,
                    "indent-normalized",
                )
                .await
            {
                return out;
            }
        }

        // ── Strategy 5: fuzzy match (skipped for replace_all) ───────────────
        if !replace_all {
            if let Some(actual) = try_fuzzy(&content, &old_str) {
                if let Some(out) = self
                    .try_apply(&call.id, &path, &content, &actual, &new_str, false, "fuzzy")
                    .await
                {
                    return out;
                }
            }
        }

        // ── All strategies failed: emit helpful suggestions ──────────────────
        let mut msg = format!("old_str not found in {path}\n\n");
        msg.push_str(
            "Tried: exact → strip-prefixes → strip-prefixes+indent → indent-normalized → fuzzy (85%)\n",
        );

        let suggestions = find_similar_blocks(&content, &old_str, MAX_SUGGESTIONS);
        if suggestions.is_empty() {
            msg.push_str(
                "\nNo similar sections found. Re-read the file to see its current content.",
            );
        } else {
            msg.push_str("\nMost similar sections in the file:\n");
            for (ratio, line_no, text) in &suggestions {
                msg.push_str(&format!(
                    "\nLine {} ({:.0}% similar):\n{}\n",
                    line_no,
                    ratio * 100.0,
                    text
                ));
            }
            msg.push_str(
                "\nTip: re-read the file and use the exact text shown above as old_str.",
            );
        }

        ToolOutput::err(&call.id, msg)
    }
}

impl EditFileTool {
    /// Apply replacement of `actual_old` with `new_str` in `content` and write
    /// the file.  Returns `None` if `actual_old` is absent (fall-through to the
    /// next strategy); returns `Some(err)` on ambiguity or write failure.
    async fn try_apply(
        &self,
        id: &str,
        path: &str,
        content: &str,
        actual_old: &str,
        new_str: &str,
        replace_all: bool,
        strategy: &str,
    ) -> Option<ToolOutput> {
        let count = content.matches(actual_old).count();
        if count == 0 {
            return None;
        }
        if count > 1 && !replace_all {
            return Some(ToolOutput::err(
                id,
                format!(
                    "old_str appears {count} times in {path}; \
                     add more surrounding context to make it unique, or use replace_all=true"
                ),
            ));
        }

        let new_content = if replace_all {
            content.replace(actual_old, new_str)
        } else {
            content.replacen(actual_old, new_str, 1)
        };

        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }

        Some(match tokio::fs::write(path, &new_content).await {
            Ok(_) => {
                let msg = if replace_all && count > 1 {
                    format!("edited {path} ({count} occurrences replaced, strategy: {strategy})")
                } else if strategy == "exact" {
                    format!("edited {path}")
                } else {
                    format!("edited {path} (strategy: {strategy})")
                };
                ToolOutput::ok(id, msg)
            }
            Err(e) => ToolOutput::err(id, format!("write error: {e}")),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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

    // ── Original tests (must continue to pass) ────────────────────────────

    #[tokio::test]
    async fn replaces_unique_string() {
        let path = tmp_file("hello world\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "world",
            "new_str": "rust"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust\n");
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
        })))
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"), "{}", out.content);
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
        })))
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("3 times"), "{}", out.content);
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

    // ── Parameter validation ──────────────────────────────────────────────

    #[tokio::test]
    async fn missing_new_str_is_error() {
        let path = tmp_file("hello\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({"path": path, "old_str": "hello"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'new_str'"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn empty_old_str_is_error() {
        let path = tmp_file("hello\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "",
            "new_str": "world"
        })))
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("must not be empty"), "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn nonexistent_file_is_read_error() {
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": "/tmp/sven_definitely_no_such_file_xyz123.txt",
            "old_str": "hello",
            "new_str": "world"
        })))
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("read error"), "{}", out.content);
    }

    // ── Strategy 2: strip L<n>: prefixes → exact ─────────────────────────

    #[tokio::test]
    async fn strips_read_file_prefixes() {
        let path = tmp_file("fn hello() {\n    println!(\"hi\");\n}\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "L1:fn hello() {\nL2:    println!(\"hi\");\nL3:}\n",
            "new_str": "fn greet() {\n    println!(\"hello\");\n}\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("strip-prefixes"), "{}", out.content);
        assert!(std::fs::read_to_string(&path).unwrap().contains("fn greet()"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn strip_prefixes_helper_basic() {
        assert_eq!(strip_read_file_prefixes("L1:hello\nL2:world\n"), "hello\nworld\n");
        // Lines without prefix are left alone
        assert_eq!(strip_read_file_prefixes("hello\nL2:world\n"), "hello\nworld\n");
        // No trailing newline preserved correctly
        assert_eq!(strip_read_file_prefixes("L10:foo"), "foo");
        // Multi-digit line numbers
        assert_eq!(strip_read_file_prefixes("L100:bar"), "bar");
    }

    #[test]
    fn strip_prefixes_does_not_strip_zero_digit_l_colon() {
        // "L:" is a valid C/Go label — must NOT be stripped (bug fix: colon > 0)
        assert_eq!(strip_read_file_prefixes("L:label_target\n"), "L:label_target\n");
    }

    // ── Strategy 3: strip-prefixes + indent-normalize (combined) ─────────

    #[tokio::test]
    async fn strips_prefixes_then_indent_normalizes() {
        // File: 4-space indented block. old_str: L<n>: prefixes + 0-indent.
        let path =
            tmp_file("    fn foo() {\n        bar();\n    }\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "L1:fn foo() {\nL2:    bar();\nL3:}\n",
            "new_str": "    fn foo() {\n        baz();\n    }\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("strip-prefixes+indent"),
            "expected strategy 'strip-prefixes+indent', got: {}",
            out.content
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("baz()"));
        let _ = std::fs::remove_file(&path);
    }

    // ── Strategy 4: indent-normalized ────────────────────────────────────

    #[tokio::test]
    async fn indent_normalized_match() {
        let path = tmp_file("    fn foo() {\n        bar();\n    }\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn foo() {\n    bar();\n}\n",
            "new_str": "    fn foo() {\n        baz();\n    }\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("indent-normalized"), "{}", out.content);
        assert!(std::fs::read_to_string(&path).unwrap().contains("baz()"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn strip_common_indent_helper() {
        assert_eq!(strip_common_indent("    foo\n    bar\n"), "foo\nbar\n");
        assert_eq!(strip_common_indent("  foo\n    bar\n"), "foo\n  bar\n");
        // Empty lines don't affect the minimum indent calculation
        assert_eq!(strip_common_indent("    foo\n\n    bar\n"), "foo\n\nbar\n");
    }

    // ── Strategy 5: fuzzy match ───────────────────────────────────────────

    #[tokio::test]
    async fn fuzzy_match_corrects_minor_typo() {
        let path =
            tmp_file("fn process_user(id: u32) {\n    validate(id);\n    update(id);\n}\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn process_user(id: usize) {\n    validate(id);\n    update(id);\n}\n",
            "new_str": "fn process_user(id: u32) {\n    validate(id);\n    update(id);\n    log(id);\n}\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("fuzzy"), "{}", out.content);
        assert!(std::fs::read_to_string(&path).unwrap().contains("log(id)"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fuzzy_does_not_match_below_threshold() {
        let path = tmp_file("fn foo() { bar(); }\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "struct Widget { name: String, value: i32, active: bool }\n",
            "new_str": "struct Widget { name: String }\n"
        })))
        .await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"), "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fuzzy_refuses_ambiguous_match() {
        // Two nearly-identical functions: fuzzy must not silently pick one.
        let path = tmp_file(concat!(
            "fn process_order(id: u32) {\n    validate(id);\n    commit(id);\n}\n",
            "fn process_payment(id: u32) {\n    validate(id);\n    commit(id);\n}\n",
        ));
        let t = EditFileTool;
        // old_str matches neither exactly but is ≥85% similar to BOTH
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn process_thing(id: u32) {\n    validate(id);\n    commit(id);\n}\n",
            "new_str": "fn process_thing(id: u32) {\n    validate(id);\n    commit(id);\n    log();\n}\n"
        })))
        .await;
        assert!(out.is_error, "expected error on ambiguous fuzzy match, got: {}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn fuzzy_skipped_for_replace_all() {
        // Only a fuzzy match exists (slight typo in old_str); replace_all=true must fail,
        // not silently apply the fuzzy match.
        let path =
            tmp_file("fn process_user(id: u32) {\n    validate(id);\n    update(id);\n}\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn process_user(id: usize) {\n    validate(id);\n    update(id);\n}\n",
            "new_str": "fn process_user(id: u32) {}\n",
            "replace_all": true
        })))
        .await;
        assert!(out.is_error, "fuzzy must be skipped for replace_all, got: {}", out.content);
        // File must be unchanged
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("validate(id)"),
            "file was mutated"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Suggestions in error output ───────────────────────────────────────

    #[tokio::test]
    async fn not_found_error_shows_suggestions() {
        let path = tmp_file(concat!(
            "fn calculate_total(items: &[Item]) -> f64 {\n",
            "    items.iter().map(|i| i.price).sum()\n",
            "}\n",
        ));
        let t = EditFileTool;
        // old_str has the right function name but a completely different body.
        // The body mismatch keeps overall similarity below the 85% fuzzy threshold,
        // so all strategies fail and the function name surfaces in the suggestions.
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn calculate_total(items: &[Item]) -> f64 {\n    items.len() as f64\n}\n",
            "new_str": "fn calculate_total(items: &[Item]) -> f64 { 0.0 }\n"
        })))
        .await;
        assert!(out.is_error, "expected all strategies to fail, got: {}", out.content);
        assert!(out.content.contains("calculate_total"), "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    // ── Context preservation ──────────────────────────────────────────────

    #[tokio::test]
    async fn surrounding_content_is_preserved() {
        let path = tmp_file("// header\nfn target() { old(); }\n// footer\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn target() { old(); }",
            "new_str": "fn target() { new(); }"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.starts_with("// header\n"), "header missing: {result}");
        assert!(result.ends_with("// footer\n"), "footer missing: {result}");
        assert!(result.contains("new()"), "replacement missing: {result}");
        assert!(!result.contains("old()"), "old content remains: {result}");
        let _ = std::fs::remove_file(&path);
    }

    // ── Stale content scenario (the original bug) ─────────────────────────

    #[tokio::test]
    async fn stale_old_str_after_successful_edit_fails_with_suggestions() {
        let path = tmp_file("fn alpha() { one(); }\nfn beta() { two(); }\n");
        let t = EditFileTool;

        // Edit 1: replace alpha's body — succeeds.
        let out1 = t.execute(&call(json!({
            "path": path,
            "old_str": "fn alpha() { one(); }",
            "new_str": "fn alpha() { updated(); }"
        })))
        .await;
        assert!(!out1.is_error, "{}", out1.content);

        // Edit 2: re-send the SAME old_str (now stale — file changed).
        let out2 = t.execute(&call(json!({
            "path": path,
            "old_str": "fn alpha() { one(); }",
            "new_str": "fn alpha() { updated(); }"
        })))
        .await;
        assert!(out2.is_error, "stale edit must fail");
        // Error must show the new content as a suggestion so the agent can recover.
        assert!(
            out2.content.contains("updated()"),
            "suggestions should show current file content: {}",
            out2.content
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── replace_all ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn replace_all_replaces_every_occurrence() {
        let path = tmp_file("foo bar foo baz foo\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "foo",
            "new_str": "qux",
            "replace_all": true
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("3 occurrences"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "qux bar qux baz qux\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn replace_all_via_strip_prefixes_strategy() {
        // replace_all=true flowing through strategy 2 (strip-prefixes).
        let path = tmp_file("fn foo() {}\nfn foo() {}\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "L1:fn foo() {}",
            "new_str": "fn bar() {}",
            "replace_all": true
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(!result.contains("fn foo()"), "both occurrences should be replaced: {result}");
        assert_eq!(result.matches("fn bar()").count(), 2);
        let _ = std::fs::remove_file(&path);
    }

    // ── Trailing newline edge cases ───────────────────────────────────────

    #[tokio::test]
    async fn old_str_with_trailing_newline_matches_file_line() {
        // old_str ends with \n; the matched text in the file does too.
        let path = tmp_file("line one\nline two\nline three\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "line two\n",
            "new_str": "line 2\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line one\nline 2\nline three\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn old_str_without_trailing_newline_matches() {
        // old_str has no trailing \n.
        let path = tmp_file("alpha\nbeta\ngamma\n");
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "beta",
            "new_str": "BETA"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha\nBETA\ngamma\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Exact beats fuzzy ────────────────────────────────────────────────

    #[tokio::test]
    async fn exact_match_takes_priority_over_fuzzy() {
        // File has the exact old_str AND a similar-but-different second block.
        // Must use exact, not fuzzy; success message must not say "fuzzy".
        let path = tmp_file(concat!(
            "fn process_user(id: u32) {\n    validate(id);\n    update(id);\n}\n",
            "fn process_admin(id: u32) {\n    validate(id);\n    elevate(id);\n}\n",
        ));
        let t = EditFileTool;
        let out = t.execute(&call(json!({
            "path": path,
            "old_str": "fn process_user(id: u32) {\n    validate(id);\n    update(id);\n}\n",
            "new_str": "fn process_user(id: u32) {\n    validate(id);\n    update(id);\n    log(id);\n}\n"
        })))
        .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            !out.content.contains("fuzzy"),
            "should have used exact, not fuzzy: {}",
            out.content
        );
        assert!(std::fs::read_to_string(&path).unwrap().contains("log(id)"));
        let _ = std::fs::remove_file(&path);
    }

    // ── similarity_ratio ──────────────────────────────────────────────────

    #[test]
    fn similarity_ratio_identical() {
        assert_eq!(similarity_ratio("hello", "hello"), 1.0);
    }

    #[test]
    fn similarity_ratio_empty() {
        assert_eq!(similarity_ratio("", ""), 1.0);
    }

    #[test]
    fn similarity_ratio_partial() {
        let r = similarity_ratio("hello world", "hello there");
        assert!(r > 0.5 && r < 1.0, "ratio={r}");
    }

    #[test]
    fn similarity_ratio_unrelated() {
        let r = similarity_ratio("aaaa", "bbbb");
        assert!(r < 0.1, "ratio={r}");
    }
}
