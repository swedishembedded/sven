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
    /// The reply states a hidden fact before the learner asked for it.
    PrematureDisclosure(String),
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
        !self
            .violations
            .iter()
            .any(|(_, v)| matches!(v, Violation::AnswerKeyLeak(_)))
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

/// Checks one reply against the spec. `learner_said` is everything the
/// learner has said so far, which is what unlocks a hidden fact.
pub(super) fn check_reply(spec: &SeatSpec, learner_said: &str, reply: &str) -> Vec<Violation> {
    let reply = normalise(reply);
    let said = normalise(learner_said);
    let mut found = Vec::new();
    for key in &spec.answer_key {
        if reply.contains(&normalise(key)) {
            found.push(Violation::AnswerKeyLeak(key.clone()));
        }
    }
    for fact in &spec.hidden_facts {
        let unlocked = fact
            .disclose_when
            .iter()
            .any(|word| said.contains(&normalise(word)));
        if !unlocked && reply.contains(&normalise(&fact.value)) {
            found.push(Violation::PrematureDisclosure(fact.id.clone()));
        }
    }
    for marker in &spec.role_breaks {
        if reply.contains(&normalise(marker)) {
            found.push(Violation::RoleBreak(marker.clone()));
        }
    }
    found
}
