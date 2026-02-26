// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sven_model::Message;
use uuid::Uuid;

/// One saved turn in the conversation log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub role: String,
    pub content: String,
}

/// In-memory conversation session.
#[derive(Debug)]
pub struct Session {
    pub id: String,
    pub messages: Vec<Message>,
    /// Approximate total token count for the current message list (chars/4).
    pub token_count: usize,
    /// Total context window in tokens (input + output) from the model catalog.
    pub max_tokens: usize,
    /// Maximum output tokens per completion from the model catalog.
    /// When non-zero, `input_budget()` subtracts this from `max_tokens` so
    /// compaction triggers before the model's hard input ceiling is reached.
    pub max_output_tokens: usize,
    /// Running calibration factor that corrects the chars/4 approximation.
    ///
    /// Updated via EMA from actual API-reported input token counts after each
    /// model turn.  Starts at 1.0 (no correction) and converges toward the
    /// true chars-per-token ratio for the current workload.
    pub calibration_factor: f32,
    /// Estimated token overhead NOT stored in `session.messages`: tool schemas
    /// and the dynamic context block (git/CI notes).  Set by the agent before
    /// each model call and included in `effective_token_count()`.
    pub schema_overhead: usize,
    /// Running total of cache-read tokens across all turns in this session.
    pub cache_read_total: u32,
    /// Running total of cache-write tokens across all turns in this session.
    pub cache_write_total: u32,
}

impl Session {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            token_count: 0,
            max_tokens,
            max_output_tokens: 0,
            calibration_factor: 1.0,
            schema_overhead: 0,
            cache_read_total: 0,
            cache_write_total: 0,
        }
    }

    /// Accumulate cache token usage from one model turn.
    pub fn add_cache_usage(&mut self, read: u32, write: u32) {
        self.cache_read_total += read;
        self.cache_write_total += write;
    }

    pub fn push(&mut self, msg: Message) {
        self.token_count += msg.approx_tokens();
        self.messages.push(msg);
    }

    pub fn push_many(&mut self, msgs: impl IntoIterator<Item = Message>) {
        for m in msgs {
            self.push(m);
        }
    }

    /// The usable input token budget: `max_tokens − max_output_tokens`.
    ///
    /// When `max_output_tokens` is unknown (0), the full `max_tokens` is
    /// returned so the caller's threshold check behaves exactly as before.
    pub fn input_budget(&self) -> usize {
        if self.max_tokens == 0 {
            return 0;
        }
        self.max_tokens.saturating_sub(self.max_output_tokens)
    }

    /// Effective token estimate: calibrated message count plus schema overhead.
    ///
    /// `calibration_factor` corrects the chars/4 approximation based on
    /// observed API input counts.  `schema_overhead` accounts for tool schemas
    /// and dynamic context that are sent with every request but not stored in
    /// `session.messages`.
    pub fn effective_token_count(&self) -> usize {
        let calibrated = (self.token_count as f32 * self.calibration_factor) as usize;
        calibrated.saturating_add(self.schema_overhead)
    }

    /// Fraction of the input budget consumed (0.0–1.0), using the calibrated
    /// effective token count.
    pub fn context_fraction(&self) -> f32 {
        let budget = self.input_budget();
        if budget == 0 {
            return 0.0;
        }
        (self.effective_token_count() as f32) / (budget as f32)
    }

    /// Return `true` when the effective token count has reached or exceeded
    /// `threshold` fraction of the input budget.
    pub fn is_near_limit(&self, threshold: f32) -> bool {
        self.context_fraction() >= threshold
    }

    /// Update the running calibration factor using an exponential moving
    /// average of the ratio between the API-reported input token count and the
    /// locally estimated value.
    ///
    /// `actual_input` is the sum of reported input + cache-read tokens for
    /// the most recent turn (the full prompt size as the provider measured it).
    /// `estimated` is the locally computed estimate at the time of the call
    /// (typically `token_count + schema_overhead` before applying calibration).
    ///
    /// The factor is clamped to [0.5, 3.0] to prevent runaway estimates.
    pub fn update_calibration(&mut self, actual_input: u32, estimated: usize) {
        if estimated == 0 || actual_input == 0 {
            return;
        }
        let ratio = actual_input as f32 / estimated as f32;
        // EMA: alpha = 0.2 → slow adaptation, resistant to spikes
        self.calibration_factor = 0.8 * self.calibration_factor + 0.2 * ratio;
        // Clamp to a sane range
        self.calibration_factor = self.calibration_factor.clamp(0.5, 3.0);
    }

    /// Recalculate token count from scratch (call after compaction).
    pub fn recalculate_tokens(&mut self) {
        self.token_count = self.messages.iter().map(|m| m.approx_tokens()).sum();
    }

    /// Replace the message list and recalculate token count (for resubmit / edit).
    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
        self.recalculate_tokens();
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use sven_model::Message;
    use super::*;

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_session_has_unique_id() {
        let a = Session::new(1000);
        let b = Session::new(1000);
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn new_session_starts_empty() {
        let s = Session::new(1000);
        assert!(s.messages.is_empty());
        assert_eq!(s.token_count, 0);
    }

    #[test]
    fn new_session_cache_totals_start_at_zero() {
        let s = Session::new(1000);
        assert_eq!(s.cache_read_total, 0);
        assert_eq!(s.cache_write_total, 0);
    }

    #[test]
    fn new_session_calibration_starts_at_one() {
        let s = Session::new(1000);
        assert!((s.calibration_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn new_session_schema_overhead_starts_at_zero() {
        let s = Session::new(1000);
        assert_eq!(s.schema_overhead, 0);
    }

    // ── Cache usage accumulation ──────────────────────────────────────────────

    #[test]
    fn add_cache_usage_accumulates_across_calls() {
        let mut s = Session::new(1000);
        s.add_cache_usage(100, 20);
        assert_eq!(s.cache_read_total, 100);
        assert_eq!(s.cache_write_total, 20);
        s.add_cache_usage(50, 10);
        assert_eq!(s.cache_read_total, 150);
        assert_eq!(s.cache_write_total, 30);
    }

    #[test]
    fn add_cache_usage_zero_args_is_noop() {
        let mut s = Session::new(1000);
        s.add_cache_usage(50, 5);
        s.add_cache_usage(0, 0);
        assert_eq!(s.cache_read_total, 50);
        assert_eq!(s.cache_write_total, 5);
    }

    #[test]
    fn add_cache_usage_read_only_leaves_write_at_zero() {
        let mut s = Session::new(1000);
        s.add_cache_usage(300, 0);
        assert_eq!(s.cache_read_total, 300);
        assert_eq!(s.cache_write_total, 0);
    }

    // ── Token accounting ──────────────────────────────────────────────────────

    #[test]
    fn push_increments_token_count() {
        let mut s = Session::new(1000);
        s.push(Message::user("12345678"));
        assert_eq!(s.token_count, 2);
    }

    #[test]
    fn push_many_accumulates_tokens() {
        let mut s = Session::new(10_000);
        s.push_many([
            Message::user("12345678"),  // 2 tokens
            Message::assistant("abcd"), // 1 token
        ]);
        assert_eq!(s.token_count, 3);
    }

    #[test]
    fn recalculate_tokens_matches_push_sum() {
        let mut s = Session::new(1000);
        s.push(Message::user("hello world")); // 11 chars → 2 tokens
        let after_push = s.token_count;
        s.recalculate_tokens();
        assert_eq!(s.token_count, after_push);
    }

    #[test]
    fn recalculate_after_manual_drain_resets_to_zero() {
        let mut s = Session::new(1000);
        s.push(Message::user("text"));
        s.messages.clear();
        s.recalculate_tokens();
        assert_eq!(s.token_count, 0);
    }

    #[test]
    fn replace_messages_sets_messages_and_recalculates_tokens() {
        let mut s = Session::new(1000);
        s.push(Message::user("first"));
        s.push(Message::assistant("reply"));
        assert_eq!(s.messages.len(), 2);
        let new_msgs = vec![Message::user("only")];
        s.replace_messages(new_msgs.clone());
        assert_eq!(s.messages.len(), 1);
        assert_eq!(s.messages[0].as_text(), Some("only"));
        assert_eq!(s.token_count, 1);
    }

    // ── Input budget ──────────────────────────────────────────────────────────

    #[test]
    fn input_budget_equals_max_tokens_when_max_output_is_zero() {
        let s = Session::new(200_000);
        assert_eq!(s.input_budget(), 200_000);
    }

    #[test]
    fn input_budget_subtracts_max_output_tokens() {
        let mut s = Session::new(200_000);
        s.max_output_tokens = 64_000;
        assert_eq!(s.input_budget(), 136_000);
    }

    #[test]
    fn input_budget_zero_when_max_tokens_zero() {
        let s = Session::new(0);
        assert_eq!(s.input_budget(), 0);
    }

    // ── Effective token count ─────────────────────────────────────────────────

    #[test]
    fn effective_token_count_equals_token_count_with_defaults() {
        let mut s = Session::new(1000);
        s.push(Message::user("12345678")); // 2 tokens
        assert_eq!(s.effective_token_count(), 2);
    }

    #[test]
    fn effective_token_count_adds_schema_overhead() {
        let mut s = Session::new(1000);
        s.push(Message::user("12345678")); // 2 tokens
        s.schema_overhead = 500;
        assert_eq!(s.effective_token_count(), 502);
    }

    #[test]
    fn effective_token_count_applies_calibration_factor() {
        let mut s = Session::new(1000);
        s.push(Message::user("12345678")); // 2 tokens
        s.calibration_factor = 1.5;
        assert_eq!(s.effective_token_count(), 3); // floor(2 * 1.5) = 3
    }

    // ── Context fraction ──────────────────────────────────────────────────────

    #[test]
    fn context_fraction_zero_when_empty() {
        let s = Session::new(1000);
        assert_eq!(s.context_fraction(), 0.0);
    }

    #[test]
    fn context_fraction_at_zero_max_does_not_panic() {
        let s = Session::new(0);
        assert_eq!(s.context_fraction(), 0.0);
    }

    #[test]
    fn context_fraction_increases_with_messages() {
        let mut s = Session::new(100);
        let before = s.context_fraction();
        s.push(Message::user("a long message that uses more tokens"));
        assert!(s.context_fraction() > before);
    }

    #[test]
    fn context_fraction_uses_input_budget_not_max_tokens() {
        let mut s = Session::new(200_000);
        s.max_output_tokens = 64_000;
        // Push 136_000 tokens worth of messages (using chars: 136_000 * 4 chars)
        // In practice, use a smaller example: push to exactly 50% of input_budget
        // input_budget = 136_000; 50% = 68_000 tokens = 272_000 chars
        // Use approx: push token_count manually
        s.token_count = 68_000;
        // context_fraction = 68_000 / 136_000 = 0.5
        let frac = s.context_fraction();
        assert!((frac - 0.5).abs() < 0.01, "fraction should be ~0.5, got {frac}");
    }

    // ── Near-limit detection ──────────────────────────────────────────────────

    #[test]
    fn is_near_limit_false_when_empty() {
        let s = Session::new(1000);
        assert!(!s.is_near_limit(0.8));
    }

    #[test]
    fn is_near_limit_true_when_over_threshold() {
        let mut s = Session::new(4);
        s.push(Message::user("1234567890123")); // 3 tokens in 4-token window = 75%
        s.push(Message::user("abcd")); // 1 more → 4 tokens, fraction = 1.0 ≥ 0.8
        assert!(s.is_near_limit(0.8));
    }

    #[test]
    fn is_near_limit_exactly_at_threshold() {
        let mut s = Session::new(10);
        s.push(Message::user("12345678901234567890")); // 20 chars = 5 tokens
        assert!(s.is_near_limit(0.5));
        assert!(!s.is_near_limit(0.6));
    }

    // ── Calibration ───────────────────────────────────────────────────────────

    #[test]
    fn update_calibration_converges_toward_actual_ratio() {
        let mut s = Session::new(1000);
        // Suppose we estimate 100 tokens but API reports 130 (ratio = 1.3)
        // After enough updates the factor should converge toward 1.3
        for _ in 0..20 {
            s.update_calibration(130, 100);
        }
        assert!(
            s.calibration_factor > 1.25 && s.calibration_factor < 1.35,
            "calibration should converge near 1.3, got {}",
            s.calibration_factor
        );
    }

    #[test]
    fn update_calibration_noop_when_estimated_zero() {
        let mut s = Session::new(1000);
        s.update_calibration(100, 0);
        assert!((s.calibration_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn update_calibration_noop_when_actual_zero() {
        let mut s = Session::new(1000);
        s.update_calibration(0, 100);
        assert!((s.calibration_factor - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn update_calibration_clamps_below_minimum() {
        let mut s = Session::new(1000);
        // Extreme: actual is 10% of estimated → would push factor below 0.5
        for _ in 0..50 {
            s.update_calibration(10, 1000);
        }
        assert!(
            s.calibration_factor >= 0.5,
            "factor must not go below 0.5, got {}",
            s.calibration_factor
        );
    }

    #[test]
    fn update_calibration_clamps_above_maximum() {
        let mut s = Session::new(1000);
        // Extreme: actual is 30× estimated → would push factor above 3.0
        for _ in 0..50 {
            s.update_calibration(30_000, 100);
        }
        assert!(
            s.calibration_factor <= 3.0,
            "factor must not exceed 3.0, got {}",
            s.calibration_factor
        );
    }
}
