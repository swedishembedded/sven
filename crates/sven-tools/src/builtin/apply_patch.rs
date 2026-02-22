// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use async_trait::async_trait;
use serde_json::{json, Value};
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct ApplyPatchTool;

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str { "apply_patch" }

    fn description(&self) -> &str {
        "Apply a patch in the sven patch format to modify, add, or delete files.\n\
         Format:\n\
         *** Begin Patch\n\
         *** Add File: path/to/new_file.rs\n\
         +content line 1\n\
         +content line 2\n\
         *** Delete File: path/to/old_file.rs\n\
         *** Update File: path/to/existing.rs\n\
         @@ context_line_1\n\
          context line (space prefix)\n\
         -removed line\n\
         +added line\n\
          context line\n\
         *** End Patch\n\
         Returns a summary of applied changes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": "The full patch text including *** Begin Patch and *** End Patch markers"
                }
            },
            "required": ["input"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let input = match call.args.get("input").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'input'"),
        };

        debug!("apply_patch tool");

        match apply_patch(&input).await {
            Ok(summary) => ToolOutput::ok(&call.id, summary),
            Err(e) => ToolOutput::err(&call.id, format!("patch error: {e}")),
        }
    }
}

async fn apply_patch(input: &str) -> anyhow::Result<String> {
    let begin = "*** Begin Patch";
    let end = "*** End Patch";

    let start = input.find(begin)
        .ok_or_else(|| anyhow::anyhow!("'*** Begin Patch' not found"))?;
    let finish = input.find(end)
        .ok_or_else(|| anyhow::anyhow!("'*** End Patch' not found"))?;

    if finish <= start {
        anyhow::bail!("'*** End Patch' appears before '*** Begin Patch'");
    }

    let body = &input[start + begin.len()..finish];
    let mut summary_lines: Vec<String> = Vec::new();

    // Parse file operations
    let mut remaining = body;

    while !remaining.trim().is_empty() {
        remaining = remaining.trim_start_matches('\n');

        if remaining.starts_with("*** Add File: ") {
            let (path, rest) = parse_file_header(remaining, "*** Add File: ")?;
            let (content, rest2) = collect_add_content(rest);
            // Create parent dirs
            if let Some(parent) = std::path::Path::new(&path).parent() {
                if !parent.as_os_str().is_empty() {
                    tokio::fs::create_dir_all(parent).await?;
                }
            }
            tokio::fs::write(&path, &content).await?;
            summary_lines.push(format!("A {path}"));
            remaining = rest2;
        } else if remaining.starts_with("*** Delete File: ") {
            let (path, rest) = parse_file_header(remaining, "*** Delete File: ")?;
            if tokio::fs::metadata(&path).await.is_ok() {
                tokio::fs::remove_file(&path).await?;
            }
            summary_lines.push(format!("D {path}"));
            remaining = rest;
        } else if remaining.starts_with("*** Update File: ") {
            let (path, rest) = parse_file_header(remaining, "*** Update File: ")?;
            let (hunks, rest2) = collect_hunks(rest);
            let file_content = tokio::fs::read_to_string(&path).await
                .map_err(|e| anyhow::anyhow!("cannot read {path}: {e}"))?;
            let new_content = apply_hunks(&file_content, &hunks)
                .map_err(|e| anyhow::anyhow!("hunk failed for {path}: {e}"))?;
            tokio::fs::write(&path, &new_content).await?;
            summary_lines.push(format!("M {path}"));
            remaining = rest2;
        } else {
            // Skip unknown lines
            let next_newline = remaining.find('\n').unwrap_or(remaining.len());
            remaining = &remaining[next_newline..];
        }
    }

    if summary_lines.is_empty() {
        Ok("(no changes applied)".to_string())
    } else {
        Ok(summary_lines.join("\n"))
    }
}

fn parse_file_header<'a>(s: &'a str, prefix: &str) -> anyhow::Result<(String, &'a str)> {
    let after_prefix = s.strip_prefix(prefix)
        .ok_or_else(|| anyhow::anyhow!("expected '{prefix}'"))?;
    let newline = after_prefix.find('\n').unwrap_or(after_prefix.len());
    let path = after_prefix[..newline].trim().to_string();
    let rest = &after_prefix[newline..];
    Ok((path, rest))
}

fn collect_add_content(s: &str) -> (String, &str) {
    let mut lines: Vec<String> = Vec::new();
    let mut remaining = s;

    loop {
        remaining = remaining.strip_prefix('\n').unwrap_or(remaining);
        if remaining.starts_with("*** ") || remaining.is_empty() {
            break;
        }
        let newline = remaining.find('\n').unwrap_or(remaining.len());
        let line = &remaining[..newline];
        if let Some(content) = line.strip_prefix('+') {
            lines.push(content.to_string());
        } else {
            lines.push(line.to_string());
        }
        remaining = &remaining[newline..];
    }

    let content = lines.join("\n");
    let content = if content.ends_with('\n') { content } else { format!("{content}\n") };
    (content, remaining)
}

#[derive(Debug)]
struct Hunk {
    /// Context lines expected before the hunk
    context_before: Vec<String>,
    /// Lines starting with '-' (to remove) and '+' (to add) and ' ' (context)
    changes: Vec<(char, String)>,
}

fn collect_hunks(s: &str) -> (Vec<Hunk>, &str) {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut remaining = s;

    loop {
        remaining = remaining.strip_prefix('\n').unwrap_or(remaining);
        if remaining.starts_with("*** ") || remaining.is_empty() {
            break;
        }

        if remaining.starts_with("@@ ") {
            // Start of a new hunk header
            let newline = remaining.find('\n').unwrap_or(remaining.len());
            let header = &remaining[3..newline].trim().to_string();
            remaining = &remaining[newline..];

            let mut context_before: Vec<String> = Vec::new();
            if !header.is_empty() {
                context_before.push(header.clone());
            }
            let mut changes: Vec<(char, String)> = Vec::new();

            // Collect hunk lines
            loop {
                remaining = remaining.strip_prefix('\n').unwrap_or(remaining);
                if remaining.starts_with("@@ ") || remaining.starts_with("*** ") || remaining.is_empty() {
                    break;
                }
                let newline = remaining.find('\n').unwrap_or(remaining.len());
                let line = &remaining[..newline];
                if let Some(rest) = line.strip_prefix('+') {
                    changes.push(('+', rest.to_string()));
                } else if let Some(rest) = line.strip_prefix('-') {
                    changes.push(('-', rest.to_string()));
                } else if let Some(rest) = line.strip_prefix(' ') {
                    changes.push((' ', rest.to_string()));
                }
                remaining = &remaining[newline..];
            }

            hunks.push(Hunk { context_before, changes });
        } else {
            let newline = remaining.find('\n').unwrap_or(remaining.len());
            remaining = &remaining[newline..];
        }
    }

    (hunks, remaining)
}

fn apply_hunks(content: &str, hunks: &[Hunk]) -> anyhow::Result<String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let had_trailing_newline = content.ends_with('\n');

    for hunk in hunks {
        // Find the hunk position using context
        let search_ctx: Vec<&str> = hunk.context_before.iter().map(String::as_str).collect();
        let expected_removes: Vec<&str> = hunk.changes.iter()
            .filter(|(c, _)| *c == '-' || *c == ' ')
            .map(|(_, l)| l.as_str())
            .collect();

        let start_pos = find_hunk_position(&lines, &search_ctx, &expected_removes)
            .ok_or_else(|| anyhow::anyhow!("could not find hunk context in file"))?;

        // Build replacement
        let mut new_section: Vec<String> = Vec::new();
        let mut i = start_pos;
        for (ch, line) in &hunk.changes {
            match ch {
                ' ' => {
                    // Context line – advance
                    i += 1;
                    new_section.push(line.clone());
                }
                '-' => {
                    // Remove line
                    i += 1;
                }
                '+' => {
                    // Add line
                    new_section.push(line.clone());
                }
                _ => {}
            }
        }

        let end_pos = i;
        lines.splice(start_pos..end_pos, new_section);
    }

    let mut result = lines.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

fn find_hunk_position(lines: &[String], context: &[&str], expected: &[&str]) -> Option<usize> {
    // Try to find position where context + expected lines match
    let search = if !context.is_empty() {
        // Find context line first
        for (i, line) in lines.iter().enumerate() {
            if line.trim() == context[0].trim() {
                // Check if subsequent lines match expected
                if lines_match_at(lines, i, expected) {
                    return Some(i);
                }
            }
        }
        return None;
    } else {
        expected
    };

    // Fallback: find expected lines without context
    (0..=lines.len().saturating_sub(search.len()))
        .find(|&i| lines_match_at(lines, i, search))
}

fn lines_match_at(lines: &[String], start: usize, expected: &[&str]) -> bool {
    if start + expected.len() > lines.len() { return false; }
    for (i, exp) in expected.iter().enumerate() {
        if lines[start + i].trim() != exp.trim() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "ap1".into(), name: "apply_patch".into(), args }
    }

    fn tmp_path(suffix: &str) -> String {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        format!("/tmp/sven_patch_test_{}_{n}{suffix}", std::process::id())
    }

    #[tokio::test]
    async fn add_new_file() {
        let path = tmp_path(".txt");
        let patch = format!(
            "*** Begin Patch\n*** Add File: {path}\n+hello\n+world\n*** End Patch\n"
        );
        let t = ApplyPatchTool;
        let out = t.execute(&call(json!({"input": patch}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains(&format!("A {path}")));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("hello"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn delete_file() {
        let path = tmp_path("_del.txt");
        std::fs::write(&path, "bye").unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Delete File: {path}\n*** End Patch\n"
        );
        let t = ApplyPatchTool;
        let out = t.execute(&call(json!({"input": patch}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains(&format!("D {path}")));
        assert!(!std::path::Path::new(&path).exists());
    }

    #[tokio::test]
    async fn update_file_with_hunk() {
        let path = tmp_path("_upd.txt");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
        let patch = format!(
            "*** Begin Patch\n*** Update File: {path}\n@@ line1\n line1\n-line2\n+line2_updated\n line3\n*** End Patch\n"
        );
        let t = ApplyPatchTool;
        let out = t.execute(&call(json!({"input": patch}))).await;
        assert!(!out.is_error, "{}", out.content);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("line2_updated"));
        assert!(!content.contains("\nline2\n"));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn missing_input_is_error() {
        let t = ApplyPatchTool;
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'input'"));
    }

    #[tokio::test]
    async fn missing_begin_marker_is_error() {
        let t = ApplyPatchTool;
        let out = t.execute(&call(json!({"input": "no markers here"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"));
    }
}
