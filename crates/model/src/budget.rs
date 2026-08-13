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
//! This module is a hard gate, not a compactor - it does not summarize or
//! shrink anything itself. Proactive compaction (`sven_machines::compact`,
//! `sven_machines::prepare_compaction`/`finish_compaction`) is wired into
//! `TurnExecutor` (`crates/executors/src/turn.rs`), which checks this
//! module's [`effective_input_budget`] against the configured
//! `compaction_threshold` *before* a turn and compacts the thread when
//! near the limit. This gate is what still fires when compaction can't
//! (or didn't) bring a request under budget - the two are complementary,
//! not redundant: compaction shrinks proactively; this rejects loudly when
//! a request is oversized regardless of why.

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

/// Minimum output tokens worth attempting a response with.
///
/// This is a *floor* for the gate, not the amount actually requested from
/// the provider — see [`dynamic_output_budget`] for that. Below this many
/// tokens of remaining room, a generation would likely be cut off before
/// saying anything useful, so the request is rejected outright rather than
/// sent and truncated. Deliberately small so a large-but-legitimate prompt
/// isn't rejected just because it doesn't leave room for the *full*
/// configured output cap — it only needs to leave room for *some* real
/// response, which [`dynamic_output_budget`] then sizes down to fit.
const MIN_OUTPUT_RESERVE: usize = 256;

/// Output-token ceiling used when nothing (config, catalog, or live probe)
/// supplies one. Matches what every provider driver except Google's already
/// falls back to internally (`anthropic.rs`/`cohere.rs`/`aws.rs`/
/// `openai_compat/mod.rs` all `unwrap_or(4096)`; only `google.rs` differs at
/// 8192) — see [`dynamic_output_budget`]'s doc comment for why this needs to
/// be a real number here rather than left to each driver.
const DEFAULT_OUTPUT_FALLBACK: u32 = 4096;

/// Fixed safety margin subtracted from the remaining room in
/// [`dynamic_output_budget`], on top of `prompt_tokens` itself.
///
/// `prompt_tokens` is a client-side chars/4 estimate of the *messages we
/// send*; it cannot see what the server's chat template adds on top -
/// role/turn markers, BOS/EOS, tool-preamble boilerplate, etc. That gap is
/// small but real and roughly *constant* per request, not proportional to
/// prompt size, so [`estimate_request_tokens`]'s multiplicative headroom
/// (1.1x) doesn't cover it: 1.1x of a 2-token "hi" is still ~2 tokens, while
/// a real chat template can easily add 8-15 tokens of its own. Confirmed in
/// practice: requesting the full theoretically-remaining output room for a
/// 2048-token window left the server needing 2056 tokens (8 over) for a
/// bare "hi" - the gap was exactly this class of invisible overhead, not a
/// client-side sizing mistake for the *content* being sent.
const CHAT_TEMPLATE_OVERHEAD_RESERVE: usize = 128;

/// The usable input budget: total context window minus the output room a
/// request actually needs to reserve.
///
/// Always reserves at least [`MIN_OUTPUT_RESERVE`] when the window is known -
/// never the full configured (or defaulted) output cap. Reserving the full
/// amount regardless of how large the prompt actually is wastes input budget
/// on every short prompt (e.g. a 2-token "hi" against a 1024-token window
/// with a 1024-token output cap would have zero usable input budget under a
/// fixed-subtraction formula, even though the prompt alone is nowhere near
/// the window). The *actual* per-request output-token limit sent to the
/// provider is computed separately by [`dynamic_output_budget`], scaled to
/// whatever room the real prompt leaves — this function only asks "is there
/// enough room left for *some* useful response at all", not "is there enough
/// room for the maximum possible one".
///
/// `None` when the context window isn't known at all (a hosted provider with
/// no catalog entry and no live probe result) — callers should skip the gate
/// entirely in that case rather than guessing a number that could be wrong
/// in either direction.
pub fn effective_input_budget(context_window: Option<u32>, _max_output_tokens: Option<u32>) -> Option<usize> {
    let window = context_window? as usize;
    Some(window.saturating_sub(MIN_OUTPUT_RESERVE))
}

/// The output-token limit to actually request for a specific call, scaled to
/// leave room for `prompt_tokens` already spent by this request instead of
/// always requesting a fixed amount regardless of prompt size *or* of what
/// the server can actually serve.
///
/// Returns `None` — meaning "no override, let the provider apply its own
/// built-in default" — only when the context window isn't known at all
/// (a hosted provider with no catalog entry and no live probe result).
/// There is nothing to scale against in that case.
///
/// When the window *is* known, this always returns `Some`, even when no
/// output cap was ever configured: the candidate ceiling is
/// `max_output_tokens.unwrap_or(DEFAULT_OUTPUT_FALLBACK)`, then narrowed to
/// whatever room `prompt_tokens` actually leaves in the window. This is the
/// fix for a real bug: a config that (correctly) carries no output cap for a
/// given provider previously meant the provider's own hardcoded ~4096-token
/// default was sent completely unchecked against a *known*, possibly much
/// smaller, live-probed or catalog context window — e.g. a 2048-token local
/// model server rejecting every request with "needs 4105 tokens" (4096
/// requested output + ~9 prompt tokens), regardless of how small the actual
/// prompt was. For a hosted provider whose context window is huge (the
/// common case for an *unconfigured* cap), this changes nothing in practice:
/// `4096.min(huge - prompt_tokens)` is still `4096`.
pub fn dynamic_output_budget(
    context_window: Option<u32>,
    max_output_tokens: Option<u32>,
    prompt_tokens: usize,
) -> Option<u32> {
    let window = context_window? as usize;
    let cap = max_output_tokens.unwrap_or(DEFAULT_OUTPUT_FALLBACK);
    let remaining = window
        .saturating_sub(prompt_tokens)
        .saturating_sub(CHAT_TEMPLATE_OVERHEAD_RESERVE);
    Some(cap.min(remaining as u32))
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
    fn budget_reserves_only_the_minimum_floor_regardless_of_configured_cap() {
        // Configured cap is 512, but only MIN_OUTPUT_RESERVE (256) is
        // actually reserved here - the rest is recovered dynamically by
        // dynamic_output_budget once the real prompt size is known.
        assert_eq!(effective_input_budget(Some(2048), Some(512)), Some(2048 - 256));
        assert_eq!(effective_input_budget(Some(2048), Some(1024)), Some(2048 - 256));
    }

    #[test]
    fn budget_reserves_the_minimum_floor_even_with_no_cap_configured() {
        // No configured cap does NOT mean "no reservation" - dynamic_output_budget
        // still derives a bounded default (DEFAULT_OUTPUT_FALLBACK) from the
        // known window, so the gate must reserve room for it too.
        assert_eq!(effective_input_budget(Some(2048), None), Some(2048 - 256));
    }

    #[test]
    fn budget_saturates_rather_than_underflows() {
        // A pathological config where even the minimum reserve exceeds the
        // window entirely must not panic or wrap around.
        assert_eq!(effective_input_budget(Some(100), Some(500)), Some(0));
    }

    #[test]
    fn budget_allows_a_tiny_prompt_against_a_small_capped_window() {
        // This is the exact bug report shape: a 1024-token window, a
        // 1024-token configured cap (reserving the *entire* window under the
        // old fixed-subtraction formula), and a 2-token "hi" prompt. It must
        // now fit.
        let budget = effective_input_budget(Some(1024), Some(1024)).unwrap();
        assert!(2 <= budget, "a 2-token prompt must fit a 1024-token window even with a 1024-token cap configured");
    }

    // ── dynamic_output_budget ────────────────────────────────────────────────

    #[test]
    fn dynamic_output_none_when_window_unknown() {
        // Nothing to scale against - the provider's own hardcoded default
        // applies, unchanged, exactly like before this function existed.
        assert_eq!(dynamic_output_budget(None, Some(1024), 10), None);
        assert_eq!(dynamic_output_budget(None, None, 10), None);
    }

    #[test]
    fn dynamic_output_derives_a_bounded_default_when_no_cap_is_configured() {
        // The exact bug this is a regression test for: a config with no
        // output cap configured for this provider (correctly - the previous
        // fix stops a DIFFERENT provider's cap from leaking in), but a KNOWN
        // (catalog or live-probed) 2048-token window. Previously this meant
        // "no override" -> the provider's own hardcoded ~4096-token default
        // was sent completely unchecked, and 4096 alone already exceeds a
        // 2048-token window regardless of how small the prompt is. Real
        // numbers from the report: server said "needs 4105 tokens" for a
        // ~9-token prompt (4096 + 9 = 4105).
        let budget = dynamic_output_budget(Some(2048), None, 9).unwrap();
        assert!(
            budget < 2048,
            "the derived default must be bounded by the known window, not the provider's raw ~4096 default, got {budget}"
        );
        assert_eq!(budget, 2048 - 9 - 128);
    }

    #[test]
    fn dynamic_output_no_cap_matches_default_fallback_for_a_huge_hosted_window() {
        // For a hosted provider's typically-huge context window, deriving a
        // default must land on the exact same ~4096 the provider would have
        // used anyway - no practical behavior change for the common case.
        assert_eq!(dynamic_output_budget(Some(200_000), None, 500), Some(4096));
    }

    #[test]
    fn dynamic_output_uses_the_full_cap_for_a_tiny_prompt() {
        // 1024-token window, 1024-token cap, 2-token prompt -> the model
        // still gets its full configured output room, minus the fixed
        // chat-template overhead reserve (128).
        assert_eq!(dynamic_output_budget(Some(1024), Some(1024), 2), Some(1024 - 2 - 128));
    }

    #[test]
    fn dynamic_output_shrinks_for_a_large_prompt() {
        assert_eq!(dynamic_output_budget(Some(2048), Some(1024), 1800), Some(2048 - 1800 - 128));
    }

    #[test]
    fn dynamic_output_never_exceeds_the_configured_cap() {
        // Plenty of room left, but the cap still wins.
        assert_eq!(dynamic_output_budget(Some(1_000_000), Some(512), 10), Some(512));
    }

    #[test]
    fn dynamic_output_saturates_rather_than_underflows() {
        assert_eq!(dynamic_output_budget(Some(1024), Some(1024), 5000), Some(0));
    }

    #[test]
    fn dynamic_output_leaves_room_for_chat_template_overhead() {
        // Regression test for the follow-up bug report: after deriving a
        // bounded default (previous test), a live server still rejected a
        // bare "hi" with "needs 2056 tokens, capacity 2048" - requesting
        // the full theoretically-remaining room left zero slack for the
        // server's own chat-template overhead (role markers etc.), which
        // sven's client-side chars/4 estimate cannot see. The returned
        // budget plus the client-side prompt estimate must leave a real
        // margin below the window, not land exactly on it.
        let prompt_tokens = 2; // client-side estimate for "hi"
        let budget = dynamic_output_budget(Some(2048), None, prompt_tokens).unwrap() as usize;
        assert!(
            prompt_tokens + budget < 2048,
            "prompt_tokens ({prompt_tokens}) + budget ({budget}) must leave real margin under the window"
        );
        assert!(
            2048 - (prompt_tokens + budget) >= CHAT_TEMPLATE_OVERHEAD_RESERVE,
            "margin must be at least CHAT_TEMPLATE_OVERHEAD_RESERVE"
        );
    }
}
