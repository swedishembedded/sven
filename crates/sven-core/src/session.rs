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
    /// Approximate total token count for the current message list
    pub token_count: usize,
    /// Maximum context tokens (set from model config / provider limits)
    pub max_tokens: usize,
}

impl Session {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            token_count: 0,
            max_tokens,
        }
    }

    pub fn push(&mut self, msg: Message) {
        self.token_count += msg.approx_tokens();
        self.messages.push(msg);
    }

    pub fn push_many(&mut self, msgs: impl IntoIterator<Item = Message>) {
        for m in msgs { self.push(m); }
    }

    /// Fraction of context window consumed (0.0–1.0)
    pub fn context_fraction(&self) -> f32 {
        if self.max_tokens == 0 { return 0.0; }
        (self.token_count as f32) / (self.max_tokens as f32)
    }

    pub fn is_near_limit(&self, threshold: f32) -> bool {
        self.context_fraction() >= threshold
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

    // ── Token accounting ──────────────────────────────────────────────────────

    #[test]
    fn push_increments_token_count() {
        let mut s = Session::new(1000);
        // "12345678" = 8 chars → 2 tokens
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
        assert_eq!(s.token_count, 1); // "only" → 1 token
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

    // ── Near-limit detection ──────────────────────────────────────────────────

    #[test]
    fn is_near_limit_false_when_empty() {
        let s = Session::new(1000);
        assert!(!s.is_near_limit(0.8));
    }

    #[test]
    fn is_near_limit_true_when_over_threshold() {
        let mut s = Session::new(4); // tiny window
        // Each char = 0.25 tokens; need 0.8 × 4 = 3.2 tokens → 13 chars
        s.push(Message::user("1234567890123")); // 13 chars = 3 tokens (floor) in 4-token window = 75%
        // Actually: 13/4 = 3 tokens; fraction = 3/4 = 0.75 < 0.8 → not near
        // Push one more to push it over
        s.push(Message::user("abcd")); // 1 more → 4 tokens, fraction = 1.0 ≥ 0.8
        assert!(s.is_near_limit(0.8));
    }

    #[test]
    fn is_near_limit_exactly_at_threshold() {
        let mut s = Session::new(10);
        // Need token_count / max_tokens ≥ threshold (0.5)
        // Fill exactly 5 tokens: 5*4=20 chars
        s.push(Message::user("12345678901234567890")); // 20 chars = 5 tokens
        assert!(s.is_near_limit(0.5));
        assert!(!s.is_near_limit(0.6));
    }
}
