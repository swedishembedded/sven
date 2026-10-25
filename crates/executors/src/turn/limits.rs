// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// running agents on local models you can procure our services by sending an
// email to info@swedishembedded.com.
//! When a turn compacts its conversation, and how large its request is.
//!
//! Compaction asks the model to write notes on the conversation, which is a
//! request of its own: the conversation, the prompt that asks for the notes,
//! and room for the notes. It has to fit the window, so the point at which
//! compaction fires is the earlier of the configured threshold and the last
//! point at which that request still fits ([`compaction_limit`]).

use std::collections::HashMap;
use std::sync::Mutex;

use sven_model::Message;

/// Chat-template overhead a request carries beyond its messages: role
/// markers, BOS/EOS, and the estimate's error on the short messages.
const REQUEST_MARGIN_TOKENS: usize = sven_turn::FORK_MARGIN_TOKENS;

/// Largest factor the server's token count may scale the estimate of a piece
/// of the conversation by; a wilder ratio means the count and the estimate
/// describe different things (a cached or truncated prompt), not an
/// estimator bias.
const MAX_CALIBRATION: f32 = 4.0;

/// Proactive-compaction settings, mirroring the agent settings'
/// `compaction_*` fields (kept as a separate small struct so `TurnExecutor`'s
/// constructor signature doesn't grow every time this feature gains a knob).
#[derive(Clone)]
pub struct CompactionConfig {
    /// Fraction of the usable input budget at which compaction fires
    /// (0.0-1.0). See `AgentConfig::compaction_threshold`.
    pub threshold: f32,
    /// Fraction of the context window reserved for tool schemas, dynamic
    /// context, and estimation error - subtracted from `threshold` to get
    /// the effective trigger point. See `AgentConfig::compaction_overhead_reserve`.
    pub overhead_reserve: f32,
    /// Fraction of the context window kept free for the model's response -
    /// reasoning included - when the conversation is judged to be full. See
    /// `AgentConfig::compaction_output_reserve`.
    pub output_reserve: f32,
    /// Most tokens the notes a compaction writes may take. See
    /// `AgentConfig::compaction_summary_tokens`.
    pub summary_tokens: u32,
}

impl Default for CompactionConfig {
    /// Matches the agent settings' default compaction fields.
    fn default() -> Self {
        Self {
            threshold: 0.98,
            overhead_reserve: 0.0,
            output_reserve: 0.02,
            summary_tokens: 10_000,
        }
    }
}

impl CompactionConfig {
    /// The fraction of the usable input budget at which compaction should
    /// fire, after accounting for overhead reserve. Never negative.
    pub(super) fn effective_threshold(&self) -> f32 {
        (self.threshold - self.overhead_reserve).max(0.0)
    }

    /// Tokens of a window of `window` tokens kept free for the response.
    pub(super) fn output_reserve_tokens(&self, window: u32) -> usize {
        (f64::from(window) * f64::from(self.output_reserve.clamp(0.0, 1.0))) as usize
    }

    /// Most tokens the notes may take in a window of `window` tokens: the
    /// configured cap, and never more than a quarter of the window, so that
    /// notes and the conversation they come from can share it.
    pub(super) fn reply_cap(&self, window: u32) -> usize {
        (self.summary_tokens as usize).min(window as usize / 4)
    }
}

/// The request size, in estimated tokens, at which the conversation is
/// compacted before the request is sent.
///
/// The earlier of two points: the configured threshold of the input budget
/// less `output_reserve` (so that a normal turn always has room to think and
/// answer), and the last size at which a request to write the notes still fits the
/// window - the conversation, `prompt_tokens` of instructions, `reply_cap`
/// tokens of reply and the template margin. Past the second point a fork
/// would have to be done in pieces.
pub(super) fn compaction_limit(
    window: u32,
    input_budget: usize,
    threshold: f32,
    output_reserve: usize,
    prompt_tokens: usize,
    reply_cap: usize,
) -> usize {
    let by_threshold = ((input_budget as f32 * threshold) as usize).saturating_sub(output_reserve);
    let by_fit = (window as usize)
        .saturating_sub(prompt_tokens)
        .saturating_sub(reply_cap)
        .saturating_sub(REQUEST_MARGIN_TOKENS);
    by_threshold.min(by_fit)
}

/// The most output tokens a response may ask for before the prompt narrows it.
///
/// Thinking is part of the response, so a local server's response may use
/// what the prompt leaves of the window; only an output cap the user
/// configured narrows that. A hosted model keeps its catalog cap.
pub(super) fn response_cap(
    model: &dyn sven_model::ModelProvider,
    context_window: Option<u32>,
) -> Option<u32> {
    model.config_max_output_tokens().or_else(|| {
        if model.is_local() {
            context_window
        } else {
            model.catalog_max_output_tokens()
        }
    })
}

/// What the server counted for the last request of a thread, and which
/// messages that request held.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Anchor {
    /// Tokens the server counted for the whole request (prompt, cache reads
    /// and writes), tools and system prompt included.
    tokens: u64,
    /// How many messages the request held.
    messages: usize,
    /// The chars/4 estimate of exactly those messages, to tell whether the
    /// conversation still starts with them.
    fingerprint: usize,
    /// How many tokens the server counted per estimated token for what was
    /// appended between the previous request and this one (1.0 until two
    /// requests have been counted). Tool output and code run well above 1.
    bias: f32,
}

/// The estimate of `raw_tokens` of appended messages, headroom included.
fn appended_estimate(raw_tokens: usize) -> usize {
    (raw_tokens * 11 / 10).max(1)
}

fn fingerprint(messages: &[Message]) -> usize {
    messages.iter().map(Message::approx_tokens).sum()
}

/// The messages of a request, as [`PromptCounts::record`] needs to know them
/// once the request's count arrives (the messages themselves are gone by
/// then).
#[derive(Clone, Copy)]
pub(super) struct SentMessages {
    messages: usize,
    fingerprint: usize,
}

impl SentMessages {
    pub(super) fn of(messages: &[Message]) -> Self {
        Self {
            messages: messages.len(),
            fingerprint: fingerprint(messages),
        }
    }
}

/// The server's token count of each thread's last request, the base of the
/// next request's estimate.
///
/// A request is the last one's messages plus what was appended since (the
/// reply, tool results, the user's next message). The server's count of the
/// first part is exact, so only the second is estimated: no scale factor can
/// inflate the estimate and fire compaction early, and a burst of tool
/// results costs only its own estimation error.
#[derive(Default)]
pub(super) struct PromptCounts(Mutex<HashMap<String, Anchor>>);

impl PromptCounts {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Anchor>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records that the request of `thread` holding the messages `sent`
    /// describes was counted at `reported` tokens. A request that reported
    /// nothing keeps the last anchor.
    pub(super) fn record(&self, thread: &str, sent: SentMessages, reported: u64) {
        if reported == 0 || sent.messages == 0 {
            return;
        }
        let mut anchors = self.map();
        let bias = anchors
            .get(thread)
            .filter(|old| {
                old.messages < sent.messages
                    && old.fingerprint < sent.fingerprint
                    && old.tokens < reported
            })
            .map_or(1.0, |old| {
                let estimated = appended_estimate(sent.fingerprint - old.fingerprint);
                ((reported - old.tokens) as f32 / estimated as f32).clamp(1.0, MAX_CALIBRATION)
            });
        anchors.insert(
            thread.to_string(),
            Anchor {
                tokens: reported,
                messages: sent.messages,
                fingerprint: sent.fingerprint,
                bias,
            },
        );
    }

    /// Forgets the count of `thread`: its conversation was rewritten.
    pub(super) fn forget(&self, thread: &str) {
        self.map().remove(thread);
    }

    /// The request size to size the response budget from: the server-backed
    /// estimate when there is a count, else the plain estimate raised by half,
    /// because the first request of a thread carries tool schemas and a chat
    /// template that chars/4 undercounts (by a third in a live run), and a
    /// response budget sized to all the room that estimate leaves is then
    /// rejected by the server.
    pub(super) fn sizing_estimate(
        &self,
        thread: &str,
        messages: &[Message],
        plain_estimate: usize,
    ) -> usize {
        let counted = self.map().contains_key(thread);
        let (estimate, _) = self.estimate(thread, messages, plain_estimate);
        if counted {
            estimate
        } else {
            estimate + estimate / 2
        }
    }

    /// Estimated tokens of a request holding `messages` plus `fixed_tokens`
    /// of tools and instructions, and the factor by which the server's counts
    /// say estimates run low (1.0 when they say nothing).
    ///
    /// With a count for the thread whose messages still lead `messages`, that
    /// count plus the estimate of the rest; otherwise the plain estimate.
    pub(super) fn estimate(
        &self,
        thread: &str,
        messages: &[Message],
        plain_estimate: usize,
    ) -> (usize, f32) {
        let anchor = self.map().get(thread).copied();
        let Some(anchor) = anchor.filter(|a| {
            a.messages <= messages.len() && fingerprint(&messages[..a.messages]) == a.fingerprint
        }) else {
            return (plain_estimate, 1.0);
        };
        let appended =
            sven_model::budget::estimate_request_tokens(&messages[anchor.messages..], &[], None);
        let estimate = anchor.tokens as usize + (appended as f32 * anchor.bias) as usize;
        let calibration = if plain_estimate == 0 {
            1.0
        } else {
            (estimate as f32 / plain_estimate as f32).clamp(1.0, MAX_CALIBRATION)
        };
        (estimate, calibration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long(chars: usize) -> Message {
        Message::user("x".repeat(chars))
    }

    // ── the trigger point ────────────────────────────────────────────────────

    #[test]
    fn a_large_window_fires_at_the_threshold() {
        // 98304 window: threshold 0.75 of the budget is far below the fit point.
        let limit = compaction_limit(98_304, 98_048, 0.85, 24_576, 600, 10_000);
        assert_eq!(limit, 83_340 - 24_576);
    }

    #[test]
    fn a_small_window_fires_where_the_fork_request_last_fits() {
        // 16000 window: notes cap 4000, prompt 600: the conversation must be
        // compacted before it exceeds 16000 - 600 - 4000 - margin.
        let limit = compaction_limit(16_000, 15_744, 0.85, 0, 600, 4_000);
        assert_eq!(limit, 16_000 - 600 - 4_000 - REQUEST_MARGIN_TOKENS);
        assert!(limit < 15_744);
    }

    #[test]
    fn at_the_limit_the_conversation_plus_the_notes_request_fits_the_window() {
        for window in [4_096u32, 8_192, 16_000, 32_768, 98_304, 262_144] {
            let cfg = CompactionConfig::default();
            let cap = cfg.reply_cap(window);
            let budget = window as usize - 256;
            let limit = compaction_limit(
                window,
                budget,
                cfg.effective_threshold(),
                cfg.output_reserve_tokens(window),
                700,
                cap,
            );
            assert!(
                limit + 700 + cap <= window as usize,
                "window {window}: limit {limit}, cap {cap}"
            );
        }
    }

    #[test]
    fn the_notes_are_capped_by_config_and_by_a_quarter_of_the_window() {
        let cfg = CompactionConfig::default();
        assert_eq!(cfg.reply_cap(98_304), 10_000);
        assert_eq!(cfg.reply_cap(16_000), 4_000);
        let wide = CompactionConfig {
            summary_tokens: 50_000,
            ..CompactionConfig::default()
        };
        assert_eq!(wide.reply_cap(98_304), 24_576);
    }

    #[test]
    fn a_window_too_small_for_a_fork_has_a_zero_limit_not_an_underflow() {
        assert_eq!(compaction_limit(500, 244, 0.75, 125, 700, 125), 0);
    }

    #[test]
    fn a_turn_always_has_the_output_reserve_left_when_compaction_does_not_fire() {
        let cfg = CompactionConfig::default();
        for window in [16_000u32, 98_304] {
            let limit = compaction_limit(
                window,
                window as usize - 256,
                cfg.effective_threshold(),
                cfg.output_reserve_tokens(window),
                700,
                cfg.reply_cap(window),
            );
            // Just under the limit the response still has the reserve (less
            // the unused slack of the fit rule).
            assert!(
                window as usize - limit >= cfg.output_reserve_tokens(window) / 2,
                "window {window}: limit {limit}"
            );
        }
    }

    #[test]
    fn a_conversation_that_still_fits_is_left_whole() {
        // Every token below the trigger is detail the episode keeps, so the
        // trigger sits at the top of the window; the request-fit bound is what
        // fires first on a window too small to hold the notes beside it.
        let cfg = CompactionConfig::default();
        assert!(cfg.effective_threshold() >= 0.95);
        let by_threshold = compaction_limit(131_072, 130_000, cfg.threshold, 0, 600, 10_000);
        let by_fit = 131_072 - 600 - 10_000 - REQUEST_MARGIN_TOKENS;
        assert_eq!(
            by_threshold, by_fit,
            "the fit bound governs on a large window"
        );
    }

    #[test]
    fn the_output_reserve_leaves_a_turn_room_at_the_top_of_the_window() {
        let cfg = CompactionConfig::default();
        assert_eq!(cfg.output_reserve_tokens(98_304), 1_966);
        assert_eq!(cfg.output_reserve_tokens(16_000), 319);
        assert_eq!(cfg.effective_threshold(), 0.98);
    }

    // ── the request estimate ─────────────────────────────────────────────────

    #[test]
    fn without_a_server_count_the_plain_estimate_stands() {
        let counts = PromptCounts::default();
        assert_eq!(counts.estimate("t", &[long(400)], 110), (110, 1.0));
    }

    #[test]
    fn a_server_count_is_the_base_and_only_what_was_appended_is_estimated() {
        let counts = PromptCounts::default();
        let sent = vec![long(400), long(400)];
        counts.record("t", SentMessages::of(&sent), 5_000);
        let mut next = sent.clone();
        next.push(long(4_000)); // 1000 tokens * 1.1
        let (estimate, _) = counts.estimate("t", &next, 9_999);
        assert_eq!(estimate, 5_000 + 1_100);
    }

    #[test]
    fn a_server_count_below_the_estimate_is_not_inflated() {
        // The chars/4 estimate over-counts: the server said 100 for what the
        // estimate puts at 220. The estimate must follow the server.
        let counts = PromptCounts::default();
        let sent = vec![long(400), long(400)];
        counts.record("t", SentMessages::of(&sent), 100);
        let (estimate, calibration) = counts.estimate("t", &sent, 220);
        assert_eq!(estimate, 100);
        assert_eq!(calibration, 1.0, "calibration never shrinks estimates");
    }

    #[test]
    fn an_estimate_that_runs_low_calibrates_fork_pieces_upwards() {
        let counts = PromptCounts::default();
        let sent = vec![long(400)];
        counts.record("t", SentMessages::of(&sent), 330); // plain estimate 110
        let (_, calibration) = counts.estimate("t", &sent, 110);
        assert!((calibration - 3.0).abs() < 0.01);
    }

    #[test]
    fn a_first_request_sizes_its_response_budget_from_a_pessimistic_estimate() {
        let counts = PromptCounts::default();
        let sent = vec![long(400)];
        assert_eq!(counts.sizing_estimate("t", &sent, 1_000), 1_500);
        counts.record("t", SentMessages::of(&sent), 700);
        assert_eq!(counts.sizing_estimate("t", &sent, 1_000), 700);
    }

    #[test]
    fn what_the_server_counted_for_the_last_appended_messages_corrects_the_next_estimate() {
        let counts = PromptCounts::default();
        let first = vec![long(400)];
        counts.record("t", SentMessages::of(&first), 1_000);
        // 4000 chars appended: estimated 1100, counted 2200.
        let mut second = first.clone();
        second.push(long(4_000));
        counts.record("t", SentMessages::of(&second), 3_200);
        // The next 4000 chars are estimated at the observed rate.
        let mut third = second.clone();
        third.push(long(4_000));
        let (estimate, _) = counts.estimate("t", &third, 0);
        assert_eq!(estimate, 3_200 + 2_200);
    }

    #[test]
    fn a_rewritten_conversation_does_not_use_the_old_count() {
        let counts = PromptCounts::default();
        counts.record("t", SentMessages::of(&[long(400), long(400)]), 5_000);
        // Same length, different content (a history rewrite elsewhere).
        let (estimate, _) = counts.estimate("t", &[long(40), long(40)], 22);
        assert_eq!(estimate, 22);
        // Shorter than the counted request.
        assert_eq!(counts.estimate("t", &[long(400)], 110).0, 110);
    }

    #[test]
    fn forgetting_a_thread_drops_its_count_and_threads_are_separate() {
        let counts = PromptCounts::default();
        let sent = vec![long(400)];
        counts.record("a", SentMessages::of(&sent), 5_000);
        counts.record("b", SentMessages::of(&sent), 7_000);
        counts.forget("a");
        assert_eq!(counts.estimate("a", &sent, 110).0, 110);
        assert_eq!(counts.estimate("b", &sent, 110).0, 7_000);
    }

    #[test]
    fn a_request_that_reported_nothing_keeps_the_last_count() {
        let counts = PromptCounts::default();
        let sent = vec![long(400)];
        counts.record("t", SentMessages::of(&sent), 5_000);
        counts.record("t", SentMessages::of(&sent), 0);
        assert_eq!(counts.estimate("t", &sent, 110).0, 5_000);
    }
}
