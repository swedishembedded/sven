// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Utility functions: format detection, artifact writing, JSON serialisation,
//! agent mode parsing, cache key sanitisation, and label normalisation.

use sven_config::AgentMode;
use sven_input::serialize_conversation_turn;
use sven_model::Message;

use crate::output::write_stderr;

use super::JsonOutput;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Return true if the markdown string looks like conversation-format output
/// (produced by `--output-format conversation`), containing recognised H2
/// section headings at line start.
///
/// This is used to detect when a prior sven run is piped into the next one so
/// the runner can parse the input as conversation history rather than as a
/// workflow, which would misinterpret `## Sven` as a workflow step label.
pub(crate) fn is_conversation_format(s: &str) -> bool {
    s.lines().any(|line| {
        let t = line.trim_end();
        matches!(t, "## User" | "## Sven" | "## Tool" | "## Tool Result")
    })
}

/// Return true if the input looks like a JSONL conversation stream: every
/// non-empty line must start with `{`.
///
/// Used to detect when `--output-format jsonl` output from a prior sven run is
/// piped into the next instance.  We inspect at most the first 10 non-empty
/// lines to keep detection fast on large streams.
pub(crate) fn is_jsonl_format(s: &str) -> bool {
    let mut checked = 0usize;
    for line in s.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if !t.starts_with('{') {
            return false;
        }
        checked += 1;
        if checked >= 10 {
            break;
        }
    }
    checked > 0
}

/// Return true if the input looks like the JSON summary produced by
/// `--output-format json`: a single JSON object containing a `"steps"` array.
///
/// Used to detect when the output of a prior `sven --output-format json` run
/// is piped into the next instance so we can reconstruct conversation history
/// from the step data instead of treating the JSON as a workflow.
pub(crate) fn is_json_summary_format(s: &str) -> bool {
    let trimmed = s.trim();
    if !trimmed.starts_with('{') {
        return false;
    }
    // Quick structural check before deserializing the full object.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        v.get("steps").and_then(|s| s.as_array()).is_some()
    } else {
        false
    }
}

/// Reconstruct a flat user/assistant `Message` history from the JSON summary
/// format produced by `--output-format json`.
///
/// Each step contributes a `user` message (`user_input`) followed by an
/// `assistant` message (`agent_response`).  Steps that have an empty
/// `agent_response` (e.g. failed steps) contribute only the user message.
pub(crate) fn parse_json_summary(s: &str) -> anyhow::Result<Vec<Message>> {
    let v: serde_json::Value = serde_json::from_str(s.trim())?;
    let steps = v
        .get("steps")
        .and_then(|s| s.as_array())
        .ok_or_else(|| anyhow::anyhow!("JSON summary missing 'steps' array"))?;

    let mut history = Vec::new();
    for step in steps {
        if let Some(user_input) = step.get("user_input").and_then(|u| u.as_str()) {
            if !user_input.is_empty() {
                history.push(Message::user(user_input));
            }
        }
        if let Some(agent_response) = step.get("agent_response").and_then(|a| a.as_str()) {
            if !agent_response.is_empty() {
                history.push(Message::assistant(agent_response));
            }
        }
    }
    Ok(history)
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

pub(super) fn write_step_artifact(
    dir: &std::path::Path,
    idx: usize,
    label: &str,
    messages: &[Message],
) {
    let safe_label = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let filename = format!("{:02}-{}.md", idx, safe_label);
    let path = dir.join(&filename);

    let content = serialize_conversation_turn(messages);
    if let Err(e) = std::fs::write(&path, &content) {
        write_stderr(&format!(
            "[sven:warn] Could not write step artifact {}: {e}",
            path.display()
        ));
    }
}

pub(super) fn write_conversation_artifact(dir: &std::path::Path, messages: &[Message]) {
    let path = dir.join("conversation.md");
    let content = serialize_conversation_turn(messages);
    if let Err(e) = std::fs::write(&path, &content) {
        write_stderr(&format!(
            "[sven:warn] Could not write conversation artifact: {e}"
        ));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

pub(super) fn json_output_to_string(out: &JsonOutput) -> String {
    let steps: Vec<serde_json::Value> = out
        .steps
        .iter()
        .map(|s| {
            serde_json::json!({
                "index": s.index,
                "label": s.label,
                "user_input": s.user_input,
                "agent_response": s.agent_response,
                "tools_used": s.tools_used,
                "duration_ms": s.duration_ms,
                "success": s.success,
            })
        })
        .collect();

    let obj = serde_json::json!({
        "title": out.title,
        "steps": steps,
    });

    serde_json::to_string_pretty(&obj)
        .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"))
}

pub(super) fn parse_agent_mode(s: &str) -> Option<AgentMode> {
    match s.trim() {
        "research" => Some(AgentMode::Research),
        "plan" => Some(AgentMode::Plan),
        "agent" => Some(AgentMode::Agent),
        _ => None,
    }
}

/// Sanitize a `cache_key` value into a safe filesystem component.
///
/// Only alphanumerics, hyphens, and underscores are kept; everything else
/// becomes `_`.  This prevents path traversal (e.g. `../../etc/passwd`) from
/// landing outside `.sven/cache/`.
pub(super) fn sanitize_cache_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Normalise a step label into a snake_case identifier suitable for use as a
/// template variable key.
///
/// ```text
/// "Gather Information" → "gather_information"
/// "Step 01: List Files" → "step_01_list_files"
/// "(unlabelled)" → "unlabelled"
/// ```
pub(super) fn normalize_label(label: &str) -> String {
    let mut result = String::new();
    let mut last_was_sep = true; // start true to avoid leading underscore
    for c in label.chars() {
        if c.is_alphanumeric() {
            for lc in c.to_lowercase() {
                result.push(lc);
            }
            last_was_sep = false;
        } else if !last_was_sep {
            result.push('_');
            last_was_sep = true;
        }
    }
    // Trim trailing underscore
    if result.ends_with('_') {
        result.pop();
    }
    result
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_label;

    #[test]
    fn spaces_become_underscores() {
        assert_eq!(normalize_label("Gather Information"), "gather_information");
    }

    #[test]
    fn numbers_preserved() {
        assert_eq!(normalize_label("Step 01: List Files"), "step_01_list_files");
    }

    #[test]
    fn parens_stripped() {
        assert_eq!(normalize_label("(unlabelled)"), "unlabelled");
    }

    #[test]
    fn already_snake_case() {
        assert_eq!(normalize_label("my_step"), "my_step");
    }
}

// resolve_model_cfg has been moved to sven_model::resolve_model_cfg.
// resolve_model_from_config (config-aware variant) lives at sven_model::resolve_model_from_config.
