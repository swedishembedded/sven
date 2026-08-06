// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Pure prompt-size budget check.
//!
//! Estimates a request's token count and compares it against the model's
//! effective input budget *before* the request is built and sent, so an
//! oversized prompt fails fast with an actionable message instead of
//! reaching the server and being rejected only after admission — or, before
//! the fail-loudly fix in [`crate::openai_compat::stream`], being silently
//! swallowed into an empty "successful" completion.
//!
//! Deliberately does NOT revive `sven-core`'s dead compaction machinery
//! (`compact_session`/`emergency_compact`, zero call sites as of this
//! writing): reviving compaction is a much larger change, and would *hide*
//! the failure this module exists to surface loudly. This is a hard gate,
//! not a compactor.

use crate::{Message, ToolSchema};

/// Headroom multiplier applied on top of the raw chars/4 estimate.
///
/// [`Message::approx_tokens`] is a heuristic, not a real tokenizer, and can
/// undercount dense text (code, non-English scripts). Erring toward a
/// slightly-too-eager rejection is far cheaper than erring toward a request
/// that reaches the server and gets rejected only after it already paid the
/// connection/admission cost.
const HEADROOM_NUM: usize = 11;
const HEADROOM_DEN: usize = 10;

/// Rough token count for a full request: message history + tool schemas +
/// an optional dynamic system-prompt suffix, all under the same chars/4
/// heuristic [`Message::approx_tokens`] uses, so the estimate is internally
/// consistent (mixing heuristics would make the headroom multiplier
/// meaningless), plus [`HEADROOM_NUM`]/[`HEADROOM_DEN`] headroom.
pub fn estimate_request_tokens(messages: &[Message], tools: &[ToolSchema], dynamic_suffix: Option<&str>) -> usize {
    let messages_tokens: usize = messages.iter().map(Message::approx_tokens).sum();
    // Tool schemas are JSON sent verbatim in every request; approximate them
    // the same chars/4 way as message text.
    let tools_tokens: usize = tools
        .iter()
        .map(|t| {
            let chars = t.name.len() + t.description.len() + t.parameters.to_string().len();
            chars.max(1).div_ceil(4)
        })
        .sum();
    let suffix_tokens = dynamic_suffix.map(|s| s.len().max(1).div_ceil(4)).unwrap_or(0);
    let raw = messages_tokens + tools_tokens + suffix_tokens;
    raw * HEADROOM_NUM / HEADROOM_DEN
}

/// The usable input budget: total context window minus reserved output
/// tokens.
///
/// `None` when the context window isn't known at all (a hosted provider with
/// no catalog entry and no live probe result) — callers should skip the gate
/// entirely in that case rather than guessing a number that could be wrong
/// in either direction.
pub fn effective_input_budget(context_window: Option<u32>, max_output_tokens: Option<u32>) -> Option<usize> {
    let window = context_window? as usize;
    let reserved_output = max_output_tokens.unwrap_or(0) as usize;
    Some(window.saturating_sub(reserved_output))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(name: &str, description: &str, params: serde_json::Value) -> ToolSchema {
        ToolSchema {
            name: name.into(),
            description: description.into(),
            parameters: params,
            is_mcp: false,
        }
    }

    // ── estimate_request_tokens ─────────────────────────────────────────────

    #[test]
    fn estimate_empty_request_is_zero() {
        assert_eq!(estimate_request_tokens(&[], &[], None), 0);
    }

    #[test]
    fn estimate_counts_message_text_with_headroom() {
        let messages = vec![Message::user("a".repeat(400))]; // 100 raw tokens
        let est = estimate_request_tokens(&messages, &[], None);
        // 100 * 11 / 10 = 110
        assert_eq!(est, 110);
    }

    #[test]
    fn estimate_counts_tool_schemas() {
        let tools = vec![schema("shell", "run a command", serde_json::json!({"type": "object"}))];
        let est = estimate_request_tokens(&[], &tools, None);
        assert!(est > 0, "tool schema JSON must contribute to the estimate");
    }

    #[test]
    fn estimate_counts_dynamic_suffix() {
        let with = estimate_request_tokens(&[], &[], Some(&"x".repeat(4000)));
        let without = estimate_request_tokens(&[], &[], None);
        assert!(with > without);
    }

    #[test]
    fn estimate_reproduces_the_brain_false_success_scenario() {
        // The actual numbers from the bug report: a "hi" turn's real prompt
        // (system prompt + tool schemas + AGENTS.md context) came to ~15000
        // tokens against a 2048-token server. A single oversized message
        // alone should already clear a 2048-token budget.
        let messages = vec![Message::user("a".repeat(60_000))]; // ~15000 raw tokens
        let est = estimate_request_tokens(&messages, &[], None);
        let budget = effective_input_budget(Some(2048), Some(0)).unwrap();
        assert!(est > budget, "estimate {est} must exceed budget {budget} for this to gate correctly");
    }

    // ── effective_input_budget ──────────────────────────────────────────────

    #[test]
    fn budget_none_when_context_window_unknown() {
        assert_eq!(effective_input_budget(None, Some(4096)), None);
    }

    #[test]
    fn budget_subtracts_reserved_output() {
        assert_eq!(effective_input_budget(Some(2048), Some(512)), Some(1536));
    }

    #[test]
    fn budget_defaults_output_reservation_to_zero_when_unset() {
        assert_eq!(effective_input_budget(Some(2048), None), Some(2048));
    }

    #[test]
    fn budget_saturates_rather_than_underflows() {
        // A pathological config where reserved output exceeds the window
        // entirely must not panic or wrap around.
        assert_eq!(effective_input_budget(Some(100), Some(500)), Some(0));
    }

    #[test]
    fn budget_matches_the_brain_default_capacity() {
        assert_eq!(effective_input_budget(Some(2048), None), Some(2048));
    }
}
