// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The automated measurement of a simulated seat: a lenient or leaky
//! simulator inflates every number measured against it, so each reply is
//! checked mechanically and the counts travel with the transcript.

use serde::{Deserialize, Serialize};

use super::simulated::SeatSpec;

/// One way a reply broke the seat's contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum Violation {
    /// The reply contains a string of the spec's answer key.
    AnswerKeyLeak(String),
    /// The reply restates an answer-key string in other words
    /// (see the `paraphrases` helper).
    AnswerKeyParaphrase(String),
    /// The reply states a hidden fact before the learner asked for it.
    PrematureDisclosure(String),
    /// The reply restates a hidden fact in other words before the learner
    /// asked for it.
    PrematureParaphrase(String),
    /// The reply steps out of the role.
    RoleBreak(String),
}

/// How a simulated seat behaved over a conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatChecks {
    /// Replies checked (the verbatim opening is not one).
    pub replies: usize,
    /// Replies with no violation.
    pub clean_replies: usize,
    /// Every violation found, with the 0-based reply it was in.
    pub violations: Vec<(usize, Violation)>,
}

impl SeatChecks {
    /// Whether the answer key never leaked. A run with a leak is not valid
    /// evidence of anything.
    #[must_use]
    pub fn leak_free(&self) -> bool {
        !self.violations.iter().any(|(_, v)| {
            matches!(
                v,
                Violation::AnswerKeyLeak(_) | Violation::AnswerKeyParaphrase(_)
            )
        })
    }

    /// Share of replies that stayed in role and within the disclosure rules.
    #[must_use]
    pub fn adherence(&self) -> f64 {
        if self.replies == 0 {
            1.0
        } else {
            self.clean_replies as f64 / self.replies as f64
        }
    }

    pub(super) fn record(&mut self, violations: Vec<Violation>) {
        let index = self.replies;
        self.replies += 1;
        if violations.is_empty() {
            self.clean_replies += 1;
        }
        self.violations
            .extend(violations.into_iter().map(|v| (index, v)));
    }
}

/// Lowercased, whitespace-collapsed, so a leak survives a line break.
fn normalise(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Checks one reply against the spec: verbatim and paraphrased leaks of the
/// answer key, hidden facts told early, and role breaks.
///
/// Checks one reply against the spec. `learner_said` is everything the
/// learner has said so far, which is what unlocks a hidden fact.
pub fn check_reply(spec: &SeatSpec, learner_said: &str, reply: &str) -> Vec<Violation> {
    let reply = normalise(reply);
    let said = normalise(learner_said);
    let mut found = Vec::new();
    for key in &spec.answer_key {
        if reply.contains(&normalise(key)) {
            found.push(Violation::AnswerKeyLeak(key.clone()));
        } else if paraphrases(&reply, key) {
            found.push(Violation::AnswerKeyParaphrase(key.clone()));
        }
    }
    for fact in &spec.hidden_facts {
        let unlocked = fact
            .disclose_when
            .iter()
            .any(|word| said.contains(&normalise(word)));
        if unlocked {
            continue;
        }
        if reply.contains(&normalise(&fact.value)) {
            found.push(Violation::PrematureDisclosure(fact.id.clone()));
        } else if paraphrases(&reply, &fact.value) {
            found.push(Violation::PrematureParaphrase(fact.id.clone()));
        }
    }
    for marker in &spec.role_breaks {
        if reply.contains(&normalise(marker)) {
            found.push(Violation::RoleBreak(marker.clone()));
        }
    }
    found
}

/// Share of a secret's content tokens that must appear close together in a
/// reply for it to count as a restatement.
pub const PARAPHRASE_COVERAGE: f64 = 0.75;

/// A secret needs at least this many content tokens for the paraphrase check;
/// a shorter one (a single name or number) is checked verbatim only, because
/// one shared word proves nothing.
pub const PARAPHRASE_MIN_TOKENS: usize = 2;

/// Words that carry no content, dropped before comparing.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "for", "from", "had", "has",
    "have", "he", "her", "his", "i", "in", "is", "it", "its", "my", "of", "on", "or", "our", "she",
    "so", "that", "the", "their", "them", "then", "there", "they", "this", "to", "was", "we",
    "were", "with", "you", "your",
];

/// Spelled-out numbers up to twenty, so "section twelve" matches "section 12".
const NUMBER_WORDS: &[&str] = &[
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

/// Content tokens of `text`: lowercase alphanumerics without stopwords,
/// number words as digits, and a crude stem (the first five characters) so
/// "deposits" and "deposited" meet.
fn content_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(w))
        .map(|w| match NUMBER_WORDS.iter().position(|n| *n == w) {
            Some(n) => n.to_string(),
            None => w.chars().take(5).collect(),
        })
        .collect()
}

/// Whether `reply` restates `secret`: some window of the reply's content
/// tokens, twice as long as the secret's, holds at least
/// [`PARAPHRASE_COVERAGE`] of the secret's distinct content tokens.
/// Deterministic and model-free; it catches reordering, inflection,
/// number words and padding, not a synonym or an inference.
fn paraphrases(reply: &str, secret: &str) -> bool {
    let wanted: std::collections::HashSet<String> = content_tokens(secret).into_iter().collect();
    if wanted.len() < PARAPHRASE_MIN_TOKENS {
        return false;
    }
    let tokens = content_tokens(reply);
    let window = 2 * wanted.len();
    let needed = (PARAPHRASE_COVERAGE * wanted.len() as f64).ceil() as usize;
    let end = tokens.len().saturating_sub(window) + 1;
    (0..end).any(|start| {
        let hit: std::collections::HashSet<&String> = tokens
            [start..(start + window).min(tokens.len())]
            .iter()
            .filter(|t| wanted.contains(*t))
            .collect();
        hit.len() >= needed
    })
}
