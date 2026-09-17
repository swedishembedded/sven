// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Structured decision envelope shared by every SDLC deliberation.
//!
//! Each SDLC state runs a deliberation that must return a JSON object matching
//! [`decision_schema`].  The `status` field is the authority signal the HSM
//! reads to choose its transition:
//!
//! | status            | machine behaviour                                   |
//! |-------------------|-----------------------------------------------------|
//! | `proceed`         | advance autonomously to the next phase              |
//! | `need_user_input` | pause; ask the developer the listed `questions`     |
//! | `need_approval`   | pause; request human approval (`approval_prompt`)   |
//! | `need_tools`      | re-deliberate once (the loop runs tools internally) |
//! | `failed`          | bubble to the Recovery superstate                   |
//!
//! Parsing is deliberately tolerant: an unknown / missing `status` is treated
//! as [`DecisionStatus::Failed`] so a malformed decision routes to recovery
//! rather than silently advancing.

use serde_json::{json, Value};

/// The authority signal a deliberation returns to the HSM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionStatus {
    /// Advance autonomously to the next phase.
    Proceed,
    /// Pause and ask the developer for clarification.
    NeedUserInput,
    /// Pause and request explicit human approval.
    NeedApproval,
    /// The model still needs tools; re-deliberate once.
    NeedTools,
    /// The deliberation failed; route to recovery.
    Failed,
}

impl DecisionStatus {
    /// Parse a status string (case-insensitive), defaulting to [`Failed`].
    ///
    /// [`Failed`]: DecisionStatus::Failed
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "proceed" => Self::Proceed,
            "need_user_input" | "need_user" | "ask_user" => Self::NeedUserInput,
            "need_approval" | "approval" => Self::NeedApproval,
            "need_tools" | "tools" => Self::NeedTools,
            _ => Self::Failed,
        }
    }
}

/// Read the `status` field of a decision value as a [`DecisionStatus`].
#[must_use]
pub fn status_of(decision: &Value) -> DecisionStatus {
    decision
        .get("status")
        .and_then(Value::as_str)
        .map(DecisionStatus::parse)
        .unwrap_or(DecisionStatus::Failed)
}

/// Extract the `questions` array (clarifications to ask the developer).
#[must_use]
pub fn questions_of(decision: &Value) -> Vec<String> {
    decision
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Extract a human-readable prompt for an approval gate, falling back to the
/// decision `summary` and finally a generic message.
#[must_use]
pub fn approval_prompt_of(decision: &Value) -> String {
    decision
        .get("approval_prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| decision.get("summary").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .unwrap_or("Approve to continue?")
        .to_string()
}

/// Extract the user-facing message / summary (for chit-chat replies and the
/// questions prompt fallback).
#[must_use]
pub fn message_of(decision: &Value) -> String {
    decision
        .get("message")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| decision.get("summary").and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

/// The phase-specific payload (plan, task list, discovery summary, …).
#[must_use]
pub fn payload_of(decision: &Value) -> Value {
    decision.get("payload").cloned().unwrap_or(Value::Null)
}

/// JSON Schema describing the shared decision envelope.  Passed to the model
/// layer as a structured-output constraint and embedded in the prompt.
#[must_use]
pub fn decision_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": true,
        "required": ["status"],
        "properties": {
            "status": {
                "type": "string",
                "enum": ["proceed", "need_user_input", "need_approval", "need_tools", "failed"],
                "description": "Authority signal that drives the state machine transition."
            },
            "summary": {
                "type": "string",
                "description": "Short summary of what was concluded in this phase."
            },
            "message": {
                "type": "string",
                "description": "User-facing text (e.g. a reply to chit-chat or a status note)."
            },
            "questions": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Clarifying questions to ask the developer (used with need_user_input)."
            },
            "approval_prompt": {
                "type": "string",
                "description": "What the developer is being asked to approve (used with need_approval)."
            },
            "payload": {
                "type": "object",
                "additionalProperties": true,
                "description": "Phase-specific structured result (discovery summary, plan, tasks, verdict)."
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_statuses() {
        assert_eq!(DecisionStatus::parse("proceed"), DecisionStatus::Proceed);
        assert_eq!(
            DecisionStatus::parse("NEED_USER_INPUT"),
            DecisionStatus::NeedUserInput
        );
        assert_eq!(
            DecisionStatus::parse("need_approval"),
            DecisionStatus::NeedApproval
        );
        assert_eq!(
            DecisionStatus::parse("need_tools"),
            DecisionStatus::NeedTools
        );
    }

    #[test]
    fn unknown_status_is_failed() {
        assert_eq!(DecisionStatus::parse("banana"), DecisionStatus::Failed);
        assert_eq!(DecisionStatus::parse(""), DecisionStatus::Failed);
    }

    #[test]
    fn status_of_missing_is_failed() {
        assert_eq!(status_of(&json!({})), DecisionStatus::Failed);
        assert_eq!(
            status_of(&json!({"status": "proceed"})),
            DecisionStatus::Proceed
        );
    }

    #[test]
    fn questions_extracted() {
        let d = json!({"status": "need_user_input", "questions": ["a?", "b?"]});
        assert_eq!(questions_of(&d), vec!["a?".to_string(), "b?".to_string()]);
    }

    #[test]
    fn approval_prompt_falls_back_to_summary() {
        let d = json!({"status": "need_approval", "summary": "do X"});
        assert_eq!(approval_prompt_of(&d), "do X");
    }

    #[test]
    fn schema_is_object_with_status_required() {
        let s = decision_schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["required"][0], "status");
    }
}
