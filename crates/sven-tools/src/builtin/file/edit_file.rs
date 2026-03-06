// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use serde_json::{json, Value};
use similar::{ChangeTag, TextDiff};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Minimum similarity ratio (0–1) for a fuzzy window to be accepted.
const FUZZY_THRESHOLD: f64 = 0.85;

// ── Hunk data structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum HunkLine {
    /// Unchanged line — must exist in the file, kept verbatim.
    Context(String),
    /// Line to remove from the file.
    Del(String),
    /// Line to insert into the file.
    Add(String),
}

#[derive(Debug, Clone)]
struct Hunk {
    /// 1-based old-file start line from `@@ -N,...` — used only as an
    /// ambiguity-breaking hint, never for primary location.
    old_start_hint: Option<usize>,
    lines: Vec<HunkLine>,
}

impl Hunk {
    /// Lines that must already be present in the file (Context + Del), in order.
    fn search_lines(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Del(s) => Some(s.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect()
    }
}

// ── Parsing ───────────────────────────────────────────────────────────────────

/// Strip a leading ` ```diff ` / ` ``` ` markdown fence if present.
fn strip_markdown_fence(diff: &str) -> &str {
    let t = diff.trim_start();
    if t.starts_with("```") {
        if let Some(nl) = t.find('\n') {
            let body = &t[nl + 1..];
            // Trim trailing closing fence
            if let Some(close) = body.rfind("\n```") {
                return &body[..close + 1];
            }
            return body;
        }
    }
    diff
}

/// Parse unified diff hunks from `diff`.
///
/// Accepts:
/// - Standard `@@ -N,M +N,M @@` headers (line numbers are optional hints)
/// - FuDiff-style `@@ @@` (no line numbers)
/// - Diffs wrapped in markdown ` ```diff ` fences
fn parse_hunks(diff: &str) -> Result<Vec<Hunk>, String> {
    let diff = strip_markdown_fence(diff);
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<Hunk> = None;

    for line in diff.lines() {
        // File header lines — skip
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        // "No newline at end of file" marker — skip
        if line.starts_with("\\ ") {
            continue;
        }
        // Hunk header
        if line.starts_with("@@") {
            if let Some(h) = current.take() {
                if !h.lines.is_empty() {
                    hunks.push(h);
                }
            }
            current = Some(Hunk {
                old_start_hint: parse_old_start(line),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(ref mut h) = current {
            if let Some(rest) = line.strip_prefix(' ') {
                h.lines.push(HunkLine::Context(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                h.lines.push(HunkLine::Del(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('+') {
                h.lines.push(HunkLine::Add(rest.to_string()));
            } else if line.is_empty() {
                // A blank diff line with no prefix = context empty line
                h.lines.push(HunkLine::Context(String::new()));
            }
            // Unknown line type — ignore
        }
    }

    if let Some(h) = current {
        if !h.lines.is_empty() {
            hunks.push(h);
        }
    }

    if hunks.is_empty() {
        return Err("No hunks found in diff. Use @@ headers.".to_string());
    }
    Ok(hunks)
}

/// Extract the 1-based old-file start line from `@@ -N[,M] +N[,M] @@`.
/// Returns `None` for FuDiff-style `@@ @@` or any unparseable header.
fn parse_old_start(header: &str) -> Option<usize> {
    // Strip leading @@ and trailing @@ section + optional function name
    let inner = header
        .trim_start_matches('@')
        .trim()
        .split("@@")
        .next()
        .unwrap_or("")
        .trim();
    // inner: "-5,7 +5,6" or "" (FuDiff)
    for part in inner.split_whitespace() {
        if let Some(rest) = part.strip_prefix('-') {
            if let Ok(n) = rest.split(',').next().unwrap_or(rest).parse::<usize>() {
                return Some(n);
            }
        }
    }
    None
}

// ── Matching helpers ──────────────────────────────────────────────────────────

/// Similarity ratio in [0,1] using character-level diff (2×matches / total).
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

/// Minimum leading-space count across non-empty lines.
fn common_indent(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0)
}

/// Strip `indent` spaces from the front of every line (trim to 0 if shorter).
fn strip_indent(lines: &[&str], indent: usize) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if l.len() >= indent {
                l[indent..].to_string()
            } else {
                l.trim_start().to_string()
            }
        })
        .collect()
}

/// Find where `search_lines` (Context + Del lines from a hunk) appear in
/// `file_lines`. Returns `(pos, indent_delta)` where:
/// - `pos`  — 0-based index of the first matching line
/// - `indent_delta` — spaces to add (positive) or remove (negative) from
///   Add lines when the match required indent normalisation or fuzzy logic
///
/// Strategies (in order):
/// 1. Exact match
/// 2. Indent-normalised match (common indent stripped on both sides)
/// 3. Fuzzy match (≥85 % combined similarity)
///
/// When multiple positions match at the same quality, `hint` (1-based old-file
/// line from the `@@ -N,...` header) picks the closest one.
fn find_hunk_position(
    file_lines: &[String],
    search_lines: &[&str],
    hint: Option<usize>,
) -> Result<(usize, i64), String> {
    // Pure insertion — no context/del lines to locate
    if search_lines.is_empty() {
        let pos = hint
            .map(|h| h.saturating_sub(1).min(file_lines.len()))
            .unwrap_or(file_lines.len());
        return Ok((pos, 0));
    }

    let n = search_lines.len();
    let file_refs: Vec<&str> = file_lines.iter().map(String::as_str).collect();

    if file_refs.len() < n {
        return Err(format!(
            "File has {} lines but hunk needs {} context/deletion lines.",
            file_refs.len(),
            n
        ));
    }

    // ── Strategy 1: exact ────────────────────────────────────────────────────
    let exact: Vec<usize> = (0..=(file_refs.len() - n))
        .filter(|&i| file_refs[i..i + n] == *search_lines)
        .collect();
    if !exact.is_empty() {
        return Ok((pick_best(&exact, hint), 0));
    }

    // ── Strategy 2: indent-normalised ────────────────────────────────────────
    let hunk_indent = common_indent(search_lines) as i64;
    let norm_search = strip_indent(search_lines, hunk_indent as usize);
    let norm_refs: Vec<&str> = norm_search.iter().map(String::as_str).collect();

    let indent_hits: Vec<(usize, i64)> = (0..=(file_refs.len() - n))
        .filter_map(|i| {
            let win = &file_refs[i..i + n];
            let file_ind = common_indent(win) as i64;
            let norm_win = strip_indent(win, file_ind as usize);
            let norm_win_refs: Vec<&str> = norm_win.iter().map(String::as_str).collect();
            if norm_win_refs == norm_refs {
                Some((i, file_ind - hunk_indent))
            } else {
                None
            }
        })
        .collect();

    if !indent_hits.is_empty() {
        let positions: Vec<usize> = indent_hits.iter().map(|(p, _)| *p).collect();
        let best = pick_best(&positions, hint);
        let delta = indent_hits
            .iter()
            .find(|(p, _)| *p == best)
            .map(|(_, d)| *d)
            .unwrap_or(0);
        return Ok((best, delta));
    }

    // ── Strategy 3: fuzzy ────────────────────────────────────────────────────
    let search_joined = search_lines.join("\n");
    let fuzzy_hits: Vec<(f64, usize, i64)> = (0..=(file_refs.len() - n))
        .filter_map(|i| {
            let win = &file_refs[i..i + n];
            let ratio = similarity_ratio(&search_joined, &win.join("\n"));
            if ratio >= FUZZY_THRESHOLD {
                let file_ind = common_indent(win) as i64;
                Some((ratio, i, file_ind - hunk_indent))
            } else {
                None
            }
        })
        .collect();

    if !fuzzy_hits.is_empty() {
        let best_ratio = fuzzy_hits
            .iter()
            .map(|(r, _, _)| *r)
            .fold(0.0_f64, f64::max);
        let best_hits: Vec<_> = fuzzy_hits
            .iter()
            .filter(|(r, _, _)| (r - best_ratio).abs() < 1e-9)
            .collect();
        let positions: Vec<usize> = best_hits.iter().map(|(_, p, _)| *p).collect();
        let best = pick_best(&positions, hint);
        let delta = best_hits
            .iter()
            .find(|(_, p, _)| *p == best)
            .map(|(_, _, d)| *d)
            .unwrap_or(0);
        return Ok((best, delta));
    }

    // ── All strategies failed — build a concise, actionable error ────────────
    let mut msg = String::from("Context not found. Expected:\n");
    for l in search_lines {
        msg.push_str(&format!("  |{l}|\n"));
    }
    let suggestions = find_similar_blocks(&file_refs, search_lines, 1);
    if let Some((ratio, line_no, block)) = suggestions.into_iter().next() {
        msg.push_str(&format!(
            "Nearest match at line {line_no} ({:.0}%):\n",
            ratio * 100.0
        ));
        for l in &block {
            msg.push_str(&format!("  |{l}|\n"));
        }
    }
    msg.push_str("Re-read the file, fix the context lines, and retry.");
    Err(msg)
}

/// When several windows match at equal quality, pick the one closest to `hint`
/// (1-based old-file line).  Falls back to the first match when hint is absent.
fn pick_best(matches: &[usize], hint: Option<usize>) -> usize {
    if matches.len() == 1 {
        return matches[0];
    }
    if let Some(h) = hint {
        let target = h.saturating_sub(1);
        return *matches
            .iter()
            .min_by_key(|&&p| (p as isize - target as isize).unsigned_abs())
            .unwrap_or(&matches[0]);
    }
    matches[0]
}

/// Return up to `limit` windows in `file_lines` most similar to `search_lines`
/// (similarity > 30 %), sorted descending.  Used for error messages.
fn find_similar_blocks(
    file_lines: &[&str],
    search_lines: &[&str],
    limit: usize,
) -> Vec<(f64, usize, Vec<String>)> {
    let n = search_lines.len().max(1);
    if file_lines.len() < n {
        return vec![];
    }
    let search_joined = search_lines.join("\n");
    let mut candidates: Vec<(f64, usize, Vec<String>)> = file_lines
        .windows(n)
        .enumerate()
        .map(|(i, win)| {
            let ratio = similarity_ratio(&search_joined, &win.join("\n"));
            (ratio, i + 1, win.iter().map(|s| s.to_string()).collect())
        })
        .filter(|(r, _, _)| *r > 0.3)
        .collect();
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(limit);
    candidates
}

// ── Hunk application ─────────────────────────────────────────────────────────

/// Adjust leading spaces on `line` by `delta` (positive = add, negative = remove).
fn adjust_indent(line: &str, delta: i64) -> String {
    if delta == 0 || line.trim().is_empty() {
        return line.to_string();
    }
    if delta > 0 {
        format!("{}{line}", " ".repeat(delta as usize))
    } else {
        let remove = (-delta) as usize;
        if line.len() >= remove && line[..remove].bytes().all(|b| b == b' ') {
            line[remove..].to_string()
        } else {
            line.trim_start_matches(' ').to_string()
        }
    }
}

/// Apply `hunk` at `pos` (0-based index where its search lines begin).
/// `indent_delta` adjusts Add lines when found via indent-normalised / fuzzy.
fn apply_hunk(file_lines: &[String], hunk: &Hunk, pos: usize, indent_delta: i64) -> Vec<String> {
    let mut result = file_lines[..pos].to_vec();
    let mut file_idx = pos;

    for hl in &hunk.lines {
        match hl {
            HunkLine::Context(_) => {
                // Keep the exact file line (preserves its real indentation).
                result.push(file_lines[file_idx].clone());
                file_idx += 1;
            }
            HunkLine::Del(_) => {
                // Skip file line — deleted.
                file_idx += 1;
            }
            HunkLine::Add(s) => {
                // Insert new line, adjusted for any indent delta.
                result.push(adjust_indent(s, indent_delta));
            }
        }
    }

    result.extend_from_slice(&file_lines[file_idx..]);
    result
}

// ── Tool ──────────────────────────────────────────────────────────────────────

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Edit a file by applying unified diff hunks.\n\
         \n\
         DIFF FORMAT\n\
         Each hunk starts with @@ (line numbers are optional hints, not required):\n\
           @@ -OLD_LINE,COUNT +NEW_LINE,COUNT @@\n\
            context line          (space prefix — unchanged)\n\
           -removed line          (minus prefix — deleted from file)\n\
           +added line            (plus prefix — inserted into file)\n\
            context line\n\
         \n\
         Rules:\n\
         • Include 2–3 unchanged context lines before and after every change.\n\
         • Context lines must match the file content exactly (indentation\n\
           differences are corrected automatically).\n\
         • Multiple @@ hunks in one diff apply changes at separate locations.\n\
         • Diffs wrapped in ```diff fences are accepted.\n\
         \n\
         Example — replace one call and add a log line:\n\
         @@ -12,6 +12,7 @@\n\
          fn process(x: u32) -> u32 {\n\
         -    x * 2\n\
         +    log(x);\n\
         +    x * 2\n\
          }\n\
         \n\
         Re-read the file after any previous edit before writing new context."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the file to edit"
                },
                "diff": {
                    "type": "string",
                    "description": "Unified diff hunks to apply. Each hunk starts with @@. \
                                    Include 2–3 context lines around every change."
                }
            },
            "required": ["path", "diff"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let path = match call.args.get("path").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err(&call.id, "Missing required parameter: path"),
        };
        let diff_str = match call.args.get("diff").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err(&call.id, "Missing required parameter: diff"),
        };

        debug!(path = %path, "edit_file tool");

        let hunks = match parse_hunks(&diff_str) {
            Ok(h) => h,
            Err(e) => return ToolOutput::err(&call.id, e),
        };

        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(&call.id, format!("read error: {e}")),
        };

        let had_trailing_newline = content.ends_with('\n');
        let mut file_lines: Vec<String> = content.lines().map(str::to_string).collect();

        for (idx, hunk) in hunks.iter().enumerate() {
            let search = hunk.search_lines();
            match find_hunk_position(&file_lines, &search, hunk.old_start_hint) {
                Ok((pos, delta)) => {
                    file_lines = apply_hunk(&file_lines, hunk, pos, delta);
                }
                Err(e) => {
                    let prefix = if hunks.len() > 1 {
                        format!("Hunk {}: ", idx + 1)
                    } else {
                        String::new()
                    };
                    return ToolOutput::err(&call.id, format!("{prefix}{e}"));
                }
            }
        }

        let mut new_content = file_lines.join("\n");
        if had_trailing_newline {
            new_content.push('\n');
        }

        if let Some(parent) = std::path::Path::new(&path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
        }

        match tokio::fs::write(&path, &new_content).await {
            Ok(_) => ToolOutput::ok(&call.id, "Edit successfully applied"),
            Err(e) => ToolOutput::err(&call.id, format!("Write failed: {e}")),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "e1".into(),
            name: "edit_file".into(),
            args,
        }
    }

    fn tmp_file(content: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/sven_edit_test_{}_{n}.txt", std::process::id());
        std::fs::write(&path, content).unwrap();
        path
    }

    // ── Parameter validation ──────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_path_is_error() {
        let t = EditFileTool;
        let out = t.execute(&call(json!({"diff": "@@ @@\n-a\n+b\n"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("path"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_diff_is_error() {
        let t = EditFileTool;
        let out = t.execute(&call(json!({"path": "/tmp/x.txt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("diff"), "{}", out.content);
    }

    #[tokio::test]
    async fn no_hunks_in_diff_is_error() {
        let path = tmp_file("hello\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(
                json!({"path": path, "diff": "just some text without @@ markers"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No hunks"), "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn nonexistent_file_is_read_error() {
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": "/tmp/sven_no_such_file_xyz.txt",
                "diff": "@@ @@\n-hello\n+world\n"
            })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("read error"), "{}", out.content);
    }

    #[test]
    fn only_available_in_agent_mode() {
        assert_eq!(EditFileTool.modes(), &[AgentMode::Agent]);
    }

    // ── Basic exact-match hunk ────────────────────────────────────────────────

    #[tokio::test]
    async fn basic_replacement() {
        let path = tmp_file("fn foo() {\n    old();\n}\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ -1,3 +1,3 @@\n fn foo() {\n-    old();\n+    new();\n }\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("new()"), "replacement missing: {result}");
        assert!(!result.contains("old()"), "old content remains: {result}");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn context_not_found_is_error() {
        let path = tmp_file("fn foo() {\n    bar();\n}\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n fn foo() {\n-    completely_different();\n+    new();\n }\n"
            })))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("Context not found"), "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn surrounding_content_is_preserved() {
        let path = tmp_file("// header\nfn target() { old(); }\n// footer\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n // header\n-fn target() { old(); }\n+fn target() { new(); }\n // footer\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(
            result.starts_with("// header\n"),
            "header missing: {result}"
        );
        assert!(result.ends_with("// footer\n"), "footer missing: {result}");
        assert!(result.contains("new()"), "replacement missing: {result}");
        assert!(!result.contains("old()"), "old content remains: {result}");
        let _ = std::fs::remove_file(&path);
    }

    // ── Trailing newline preservation ─────────────────────────────────────────

    #[tokio::test]
    async fn trailing_newline_preserved() {
        let path = tmp_file("line one\nline two\nline three\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n line one\n-line two\n+line 2\n line three\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.ends_with('\n'), "trailing newline lost");
        assert_eq!(result, "line one\nline 2\nline three\n");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn no_trailing_newline_preserved() {
        let path = tmp_file("alpha\nbeta\ngamma");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n alpha\n-beta\n+BETA\n gamma\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(!result.ends_with('\n'), "unexpected trailing newline");
        assert_eq!(result, "alpha\nBETA\ngamma");
        let _ = std::fs::remove_file(&path);
    }

    // ── Multi-hunk ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn multi_hunk_applies_both_changes() {
        let path =
            tmp_file("use std::io;\n\nfn alpha() {\n    a();\n}\n\nfn beta() {\n    b();\n}\n");
        let t = EditFileTool;
        let diff = concat!(
            "@@ @@\n",
            " fn alpha() {\n",
            "-    a();\n",
            "+    alpha_new();\n",
            " }\n",
            "@@ @@\n",
            " fn beta() {\n",
            "-    b();\n",
            "+    beta_new();\n",
            " }\n",
        );
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("Edit successfully applied"),
            "{}",
            out.content
        );
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("alpha_new()"), "{result}");
        assert!(result.contains("beta_new()"), "{result}");
        let _ = std::fs::remove_file(&path);
    }

    // ── Pure insertion ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn pure_insertion_with_context() {
        let path = tmp_file("fn foo() {\n    existing();\n}\n");
        let t = EditFileTool;
        // Insert a new line after fn foo() {
        let diff = "@@ @@\n fn foo() {\n+    new_line();\n     existing();\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("new_line()"), "{result}");
        assert!(result.contains("existing()"), "{result}");
        let _ = std::fs::remove_file(&path);
    }

    // ── Pure deletion ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn pure_deletion() {
        let path = tmp_file("line1\nremove_me\nline3\n");
        let t = EditFileTool;
        let diff = "@@ @@\n line1\n-remove_me\n line3\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(!result.contains("remove_me"), "{result}");
        assert_eq!(result, "line1\nline3\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Indent normalisation ──────────────────────────────────────────────────

    #[tokio::test]
    async fn indent_normalised_match() {
        // File uses 4-space indent; hunk uses 0-indent (LLM stripped leading spaces)
        let path = tmp_file("    fn foo() {\n        old();\n    }\n");
        let t = EditFileTool;
        let diff = "@@ @@\n fn foo() {\n-    old();\n+    new();\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("new()"), "{result}");
        assert!(!result.contains("old()"), "{result}");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn add_lines_indented_when_indent_normalised() {
        // File is 4-space indented; hunk has 0 indent on context and Add lines.
        // The added line must be emitted with 4 extra spaces.
        let path = tmp_file("    fn foo() {\n        bar();\n    }\n");
        let t = EditFileTool;
        let diff = "@@ @@\n fn foo() {\n-    bar();\n+    baz();\n+    qux();\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        // Added lines should carry the same 4-space indent as the file block
        assert!(
            result.contains("        baz();"),
            "expected 8-space indent on baz: {result}"
        );
        assert!(
            result.contains("        qux();"),
            "expected 8-space indent on qux: {result}"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Fuzzy match ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn fuzzy_match_corrects_minor_typo_in_context() {
        // Context has "u32" but file has "u64" — close enough for fuzzy.
        let path = tmp_file("fn process(id: u64) {\n    validate(id);\n    update(id);\n}\n");
        let t = EditFileTool;
        let diff =
            "@@ @@\n fn process(id: u32) {\n     validate(id);\n-    update(id);\n+    update(id);\n+    log(id);\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("log(id)"),
            "insertion missing"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Line number hint resolves ambiguity ───────────────────────────────────

    #[tokio::test]
    async fn line_number_hint_picks_correct_duplicate() {
        // File has two identical blocks; hint selects the second one.
        let path = tmp_file(concat!(
            "fn block() {\n    value = 1;\n}\n\n",
            "fn block() {\n    value = 1;\n}\n",
        ));
        let t = EditFileTool;
        // Second block starts at line 5; hint points there.
        let diff = "@@ -5,3 +5,3 @@\n fn block() {\n-    value = 1;\n+    value = 2;\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        let result = std::fs::read_to_string(&path).unwrap();
        // First block unchanged, second updated
        let first = result.find("value = 1;").unwrap();
        let second = result.find("value = 2;").unwrap();
        assert!(
            first < second,
            "second block should have been updated: {result}"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── FuDiff style (@@ @@ no numbers) ──────────────────────────────────────

    #[tokio::test]
    async fn fudiff_header_without_line_numbers() {
        let path = tmp_file("hello world\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n-hello world\n+hello rust\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Markdown-fenced diff ──────────────────────────────────────────────────

    #[tokio::test]
    async fn markdown_fenced_diff_is_accepted() {
        let path = tmp_file("fn foo() { bar(); }\n");
        let t = EditFileTool;
        let diff = "```diff\n@@ @@\n-fn foo() { bar(); }\n+fn foo() { baz(); }\n```\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("baz()"),
            "replacement missing"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Error: context not found shows suggestions ────────────────────────────

    #[tokio::test]
    async fn not_found_error_shows_similar_section() {
        let path = tmp_file(
            "fn calculate_total(items: &[Item]) -> f64 {\n    items.iter().map(|i| i.price).sum()\n}\n",
        );
        let t = EditFileTool;
        // Context has right function name but completely wrong body
        let diff = concat!(
            "@@ @@\n",
            " fn calculate_total(items: &[Item]) -> f64 {\n",
            "-    items.len() as f64\n",
            "+    0.0\n",
            " }\n",
        );
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(out.is_error, "expected error");
        assert!(
            out.content.contains("calculate_total"),
            "suggestion should mention function: {}",
            out.content
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Stale context detected ────────────────────────────────────────────────

    #[tokio::test]
    async fn stale_context_after_edit_fails_with_suggestions() {
        let path = tmp_file("fn alpha() { one(); }\nfn beta() { two(); }\n");
        let t = EditFileTool;

        // First edit — succeeds
        let out1 = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n fn alpha() { one(); }\n-fn alpha() { one(); }\n+fn alpha() { updated(); }\n"
            })))
            .await;
        // That hunk is self-referential so let's use a simpler approach
        let _ = out1;
        let _ = std::fs::remove_file(&path);

        let path2 = tmp_file("fn alpha() { one(); }\nfn beta() { two(); }\n");
        let out_a = t
            .execute(&call(json!({
                "path": path2,
                "diff": "@@ @@\n-fn alpha() { one(); }\n+fn alpha() { updated(); }\n"
            })))
            .await;
        assert!(!out_a.is_error, "{}", out_a.content);

        // Second call with the OLD context — must fail and show suggestions
        let out_b = t
            .execute(&call(json!({
                "path": path2,
                "diff": "@@ @@\n-fn alpha() { one(); }\n+fn alpha() { updated(); }\n"
            })))
            .await;
        assert!(out_b.is_error, "stale context must fail");
        assert!(
            out_b.content.contains("updated()"),
            "suggestion should show current content: {}",
            out_b.content
        );
        let _ = std::fs::remove_file(&path2);
    }

    // ── parse_old_start unit tests ────────────────────────────────────────────

    #[test]
    fn parse_old_start_standard() {
        assert_eq!(parse_old_start("@@ -5,7 +5,6 @@"), Some(5));
        assert_eq!(parse_old_start("@@ -1,3 +1,3 @@"), Some(1));
        assert_eq!(parse_old_start("@@ -9,3 +8,6 @@ fn main()"), Some(9));
    }

    #[test]
    fn parse_old_start_single_line() {
        assert_eq!(parse_old_start("@@ -5 +5 @@"), Some(5));
    }

    #[test]
    fn parse_old_start_fudiff() {
        assert_eq!(parse_old_start("@@ @@"), None);
        assert_eq!(parse_old_start("@@"), None);
    }

    // ── similarity_ratio unit tests ───────────────────────────────────────────

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

    // ── strip_markdown_fence unit test ────────────────────────────────────────

    #[test]
    fn strip_fence_removes_backticks() {
        let fenced = "```diff\n@@ @@\n-old\n+new\n```\n";
        let stripped = strip_markdown_fence(fenced);
        assert!(!stripped.contains("```"), "fences not removed: {stripped}");
        assert!(stripped.contains("@@"), "hunk missing: {stripped}");
    }

    #[test]
    fn strip_fence_no_op_when_no_fence() {
        let plain = "@@ @@\n-old\n+new\n";
        assert_eq!(strip_markdown_fence(plain), plain);
    }

    // ── adjust_indent unit tests ──────────────────────────────────────────────

    #[test]
    fn adjust_indent_add() {
        assert_eq!(adjust_indent("    foo", 4), "        foo");
    }

    #[test]
    fn adjust_indent_remove() {
        assert_eq!(adjust_indent("        foo", -4), "    foo");
    }

    #[test]
    fn adjust_indent_zero_noop() {
        assert_eq!(adjust_indent("    foo", 0), "    foo");
    }

    #[test]
    fn adjust_indent_empty_line_noop() {
        assert_eq!(adjust_indent("", 4), "");
    }

    #[test]
    fn adjust_indent_remove_more_than_available_trims_to_zero() {
        // Trying to remove 8 spaces from a 4-space-indented line should not panic.
        assert_eq!(adjust_indent("    foo", -8), "foo");
    }

    // ── Success message ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn success_message_is_edit_successfully_applied() {
        let path = tmp_file("a\nb\nc\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n a\n-b\n+B\n c\n"
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "Edit successfully applied");
        let _ = std::fs::remove_file(&path);
    }

    // ── Diff with --- / +++ file headers ─────────────────────────────────────

    #[tokio::test]
    async fn diff_with_file_headers_is_accepted() {
        let path = tmp_file("fn foo() { old(); }\n");
        let t = EditFileTool;
        let diff = "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1 +1 @@\n-fn foo() { old(); }\n+fn foo() { new(); }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn foo() { new(); }\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Git extended header with section name ─────────────────────────────────

    #[tokio::test]
    async fn git_extended_header_with_section_name() {
        let path = tmp_file("fn greet() {\n    old();\n}\n");
        let t = EditFileTool;
        // @@ -1,3 +1,3 @@ fn greet() — section name after second @@
        let diff = "@@ -1,3 +1,3 @@ fn greet()\n fn greet() {\n-    old();\n+    new();\n }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(std::fs::read_to_string(&path).unwrap().contains("new()"));
        let _ = std::fs::remove_file(&path);
    }

    // ── No-newline marker ignored ─────────────────────────────────────────────

    #[tokio::test]
    async fn no_newline_marker_is_ignored() {
        let path = tmp_file("old\n");
        let t = EditFileTool;
        let diff = "@@ @@\n-old\n+new\n\\ No newline at end of file\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Change at start of file ───────────────────────────────────────────────

    #[tokio::test]
    async fn change_at_start_of_file() {
        let path = tmp_file("first\nsecond\nthird\n");
        let t = EditFileTool;
        let diff = "@@ -1,2 +1,2 @@\n-first\n+FIRST\n second\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "FIRST\nsecond\nthird\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Change at end of file ─────────────────────────────────────────────────

    #[tokio::test]
    async fn change_at_end_of_file() {
        let path = tmp_file("first\nsecond\nlast\n");
        let t = EditFileTool;
        let diff = "@@ @@\n second\n-last\n+LAST\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nsecond\nLAST\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Single-line file ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn single_line_file() {
        let path = tmp_file("only line\n");
        let t = EditFileTool;
        let diff = "@@ @@\n-only line\n+changed line\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed line\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Multi-line deletion ───────────────────────────────────────────────────

    #[tokio::test]
    async fn multi_line_deletion() {
        let path = tmp_file("keep1\ndelete_a\ndelete_b\ndelete_c\nkeep2\n");
        let t = EditFileTool;
        let diff = "@@ @@\n keep1\n-delete_a\n-delete_b\n-delete_c\n keep2\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep1\nkeep2\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Multi-line insertion ──────────────────────────────────────────────────

    #[tokio::test]
    async fn multi_line_insertion() {
        let path = tmp_file("before\nafter\n");
        let t = EditFileTool;
        let diff = "@@ @@\n before\n+added_1\n+added_2\n+added_3\n after\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "before\nadded_1\nadded_2\nadded_3\nafter\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Complex mixed hunk ────────────────────────────────────────────────────

    #[tokio::test]
    async fn complex_mixed_hunk_del_and_add_interleaved() {
        let path = tmp_file("a\nb\nc\nd\ne\n");
        let t = EditFileTool;
        // Replace b with B, keep c, replace d with D
        let diff = "@@ @@\n a\n-b\n+B\n c\n-d\n+D\n e\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nB\nc\nD\ne\n");
        let _ = std::fs::remove_file(&path);
    }

    // ── Three-hunk diff ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn three_hunk_diff() {
        let path = tmp_file("aa\nbb\ncc\ndd\nee\nff\ngg\n");
        let t = EditFileTool;
        let diff = concat!(
            "@@ @@\n-aa\n+AA\n bb\n",
            "@@ @@\n cc\n-dd\n+DD\n ee\n",
            "@@ @@\n ff\n-gg\n+GG\n",
        );
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "AA\nbb\ncc\nDD\nee\nff\nGG\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Multi-hunk: second fails → file unchanged, error names hunk ──────────

    #[tokio::test]
    async fn second_hunk_failure_names_hunk_and_file_is_unchanged() {
        let path = tmp_file("line1\nline2\nline3\n");
        let t = EditFileTool;
        let diff = concat!(
            "@@ @@\n-line1\n+LINE1\n line2\n", // hunk 1: valid
            "@@ @@\n-does_not_exist\n+X\n",    // hunk 2: bad context
        );
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(out.is_error, "expected error");
        assert!(
            out.content.contains("Hunk 2"),
            "error should name failed hunk: {}",
            out.content
        );
        // File must be completely unchanged — hunks are applied atomically
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line1\nline2\nline3\n",
            "file was modified despite failure"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Single-hunk failure: no "Hunk N:" prefix ─────────────────────────────

    #[tokio::test]
    async fn single_hunk_failure_has_no_hunk_prefix() {
        let path = tmp_file("hello\n");
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n-does_not_exist\n+x\n"
            })))
            .await;
        assert!(out.is_error);
        assert!(
            !out.content.starts_with("Hunk"),
            "single-hunk error should not have 'Hunk N:' prefix: {}",
            out.content
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── File unchanged on read error ──────────────────────────────────────────

    #[tokio::test]
    async fn file_unchanged_when_context_not_found() {
        let original = "line1\nline2\nline3\n";
        let path = tmp_file(original);
        let t = EditFileTool;
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n-no_such_line\n+replacement\n"
            })))
            .await;
        assert!(out.is_error);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "file was modified despite failure"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Fuzzy below threshold fails ───────────────────────────────────────────

    #[tokio::test]
    async fn fuzzy_below_threshold_fails() {
        let path = tmp_file("fn foo() { completely_different_content_here(); }\n");
        let t = EditFileTool;
        // Context shares almost nothing with the file — well below 85%
        let out = t
            .execute(&call(json!({
                "path": path,
                "diff": "@@ @@\n-struct Widget { name: String, value: i32, active: bool }\n+struct Widget { name: String }\n"
            })))
            .await;
        assert!(out.is_error, "{}", out.content);
        let _ = std::fs::remove_file(&path);
    }

    // ── Blank context line in hunk ────────────────────────────────────────────

    #[tokio::test]
    async fn blank_context_line_in_hunk() {
        // The blank line between the two functions must be treated as context.
        let path = tmp_file("fn a() {}\n\nfn b() {}\n");
        let t = EditFileTool;
        let diff = "@@ @@\n fn a() {}\n \n-fn b() {}\n+fn b() { /* new */ }\n";
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("/* new */"),
            "replacement missing"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Offset tracking: second hunk targets post-edit position ──────────────

    #[tokio::test]
    async fn multi_hunk_offset_tracking() {
        // Hunk 1 inserts 2 lines after "insert_after".
        // Hunk 2 targets "target" which now sits 2 lines lower — context matching
        // must find it correctly in the updated in-memory content.
        let path = tmp_file("insert_after\ntarget\nend\n");
        let t = EditFileTool;
        let diff = concat!(
            "@@ @@\n insert_after\n+new1\n+new2\n target\n",
            "@@ @@\n-target\n+TARGET\n end\n",
        );
        let out = t.execute(&call(json!({"path": path, "diff": diff}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "insert_after\nnew1\nnew2\nTARGET\nend\n"
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── parse_hunks unit tests ────────────────────────────────────────────────

    #[test]
    fn parse_hunks_returns_correct_types() {
        let diff = "@@ @@\n context\n-deleted\n+added\n context2\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        let lines = &hunks[0].lines;
        assert!(matches!(&lines[0], HunkLine::Context(s) if s == "context"));
        assert!(matches!(&lines[1], HunkLine::Del(s) if s == "deleted"));
        assert!(matches!(&lines[2], HunkLine::Add(s) if s == "added"));
        assert!(matches!(&lines[3], HunkLine::Context(s) if s == "context2"));
    }

    #[test]
    fn parse_hunks_multi_hunk_count() {
        let diff = "@@ @@\n-a\n+A\n@@ @@\n-b\n+B\n@@ @@\n-c\n+C\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks.len(), 3);
    }

    #[test]
    fn parse_hunks_empty_hunk_body_is_skipped() {
        // A @@ header with no body lines should not produce a hunk
        let diff = "@@ @@\n@@ @@\n-a\n+b\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks.len(), 1, "empty hunk should be skipped");
    }

    #[test]
    fn parse_hunks_file_header_lines_are_ignored() {
        let diff = "--- a/foo.rs\n+++ b/foo.rs\n@@ @@\n-old\n+new\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines.len(), 2);
    }

    #[test]
    fn parse_hunks_no_newline_marker_is_ignored() {
        let diff = "@@ @@\n-old\n+new\n\\ No newline at end of file\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(
            hunks[0].lines.len(),
            2,
            "marker should not become a hunk line"
        );
    }

    #[test]
    fn parse_hunks_extracts_old_start_hint() {
        let diff = "@@ -42,5 +42,6 @@\n-a\n+b\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks[0].old_start_hint, Some(42));
    }

    #[test]
    fn parse_hunks_fudiff_has_no_hint() {
        let diff = "@@ @@\n-a\n+b\n";
        let hunks = parse_hunks(diff).unwrap();
        assert_eq!(hunks[0].old_start_hint, None);
    }

    // ── strip_markdown_fence edge cases ──────────────────────────────────────

    #[test]
    fn strip_fence_no_closing_fence_returns_body() {
        // LLM sometimes emits opening fence but no closing one
        let fenced = "```diff\n@@ @@\n-old\n+new\n";
        let stripped = strip_markdown_fence(fenced);
        assert!(stripped.contains("@@"), "hunk missing: {stripped}");
        assert!(
            !stripped.contains("```"),
            "opening fence not removed: {stripped}"
        );
    }

    #[test]
    fn strip_fence_plain_backticks_without_diff_label() {
        let fenced = "```\n@@ @@\n-old\n+new\n```\n";
        let stripped = strip_markdown_fence(fenced);
        assert!(stripped.contains("@@"), "hunk missing: {stripped}");
        assert!(!stripped.contains("```"), "fences not removed: {stripped}");
    }

    // ── common_indent / strip_indent unit tests ───────────────────────────────

    #[test]
    fn common_indent_all_empty_lines_is_zero() {
        let lines: &[&str] = &["", "  ", "\t"];
        assert_eq!(common_indent(lines), 0);
    }

    #[test]
    fn common_indent_mixed() {
        let lines: &[&str] = &["    foo", "        bar", "    baz"];
        assert_eq!(common_indent(lines), 4);
    }

    #[test]
    fn strip_indent_removes_common() {
        let lines: &[&str] = &["    foo", "        bar"];
        let result = strip_indent(lines, 4);
        assert_eq!(result, vec!["foo", "    bar"]);
    }
}
