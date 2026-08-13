// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Utility functions: format detection, artifact writing, JSON serialisation,
//! agent mode parsing, cache key sanitisation, and label normalisation.

use anyhow::Context;
use sven_config::AgentMode;
use sven_session_store::serialize_conversation_turn;
use sven_model::Message;

use crate::output::write_stderr;

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

/// Return true if the input looks like an NDJSON stream of ATIF `TraceStep`
/// objects: every non-empty line must start with `{`.
///
/// Used to detect when `--output-format jsonl` output from a prior sven run
/// is piped into the next instance — each line is a standalone `TraceStep`
/// JSON object (see [`atif::persist::read_steps_ndjson`]), not the whole
/// `Trajectory` document `--output-trace`/`--trace` write to a file.  We
/// inspect at most the first 10 non-empty lines to keep detection fast on
/// large streams.
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

/// Parse an NDJSON stream of ATIF `TraceStep` objects (as detected by
/// [`is_jsonl_format`] / produced by `--output-format jsonl`) into a message
/// history and any trailing pending user turn.
///
/// Mirrors the old `sven_session_store::parse_jsonl_full`'s `(history,
/// pending_user_input)` contract for the `ConversationRecord` format: if the
/// last step is a `User`-source step, it is treated as not yet answered and
/// stripped from `history` into `pending_user_input`; otherwise `history`
/// covers every step and there is no pending turn.
pub(crate) fn parse_jsonl_trace_steps(s: &str) -> anyhow::Result<(Vec<Message>, Option<String>)> {
    let steps = atif::persist::read_steps_ndjson(std::io::Cursor::new(s.as_bytes()))
        .context("parsing piped NDJSON trace steps")?;

    let (history_steps, pending): (&[atif::TraceStep], Option<String>) = match steps.last() {
        Some(step) if step.source == atif::StepOrigin::User => {
            let pending = step.message.as_text().unwrap_or("").to_string();
            (&steps[..steps.len() - 1], Some(pending))
        }
        _ => (&steps[..], None),
    };

    let history = sven_session_store::trace_session::steps_to_messages(history_steps);
    Ok((history, pending))
}

/// Return true if the input looks like the JSON output produced by
/// `--output-format json`: a single JSON object containing a `"steps"` array
/// (an ATIF `Trajectory` document).
///
/// Used to detect when the output of a prior `sven --output-format json` run
/// is piped into the next instance so we can reconstruct conversation history
/// from the trajectory's steps instead of treating the JSON as a workflow.
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

/// Reconstruct a flat `Message` history from the JSON output produced by
/// `--output-format json`: a pretty-printed ATIF `Trajectory` document.
///
/// Delegates to [`sven_session_store::trace_session::steps_to_messages`] so the
/// reconstruction rules (system/copied-context steps skipped, tool calls
/// un-merged from their observations, reasoning never replayed) match every
/// other trace-consuming path in the runner.
pub(crate) fn parse_json_summary(s: &str) -> anyhow::Result<Vec<Message>> {
    let trajectory: atif::Trajectory = serde_json::from_str(s.trim())
        .context("parsing --output-format json output as an ATIF trajectory")?;
    Ok(sven_session_store::trace_session::steps_to_messages(
        &trajectory.steps,
    ))
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
