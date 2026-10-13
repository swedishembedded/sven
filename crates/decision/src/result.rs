// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! What a decision model answers.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{DecisionError, QuestionKind};

/// One candidate of a question with the probability the model gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    /// Position in the list the caller sent.
    pub index: usize,
    pub probability: f32,
}

/// The full ranked distribution of one question, best first. Ties are ordered
/// by position in the caller's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Distribution {
    /// Every candidate, never only the winner. Never empty.
    pub ranked: Vec<Candidate>,
    /// The model's own confidence, when it reports one.
    pub confidence: Option<f32>,
    /// The model answered from a prefix of the state.
    pub state_truncated: bool,
}

impl Distribution {
    /// A distribution of `(id, probability)` in the caller's order, ranked.
    ///
    /// # Errors
    ///
    /// [`DecisionError::InvalidResponse`] when there is no candidate or a
    /// probability is not a number in `0..=1`.
    pub fn from_candidates(candidates: &[(&str, f32)]) -> Result<Self, DecisionError> {
        let mut ranked = Vec::with_capacity(candidates.len());
        for (index, (id, probability)) in candidates.iter().enumerate() {
            if !(0.0..=1.0).contains(probability) {
                return Err(DecisionError::InvalidResponse(format!(
                    "candidate {id:?} has probability {probability}, outside 0..=1"
                )));
            }
            ranked.push(Candidate {
                id: (*id).to_string(),
                index,
                probability: *probability,
            });
        }
        Self::ranked(ranked)
    }

    /// A distribution of candidates that already carry their positions.
    ///
    /// # Errors
    ///
    /// As [`Self::from_candidates`].
    pub fn ranked(mut candidates: Vec<Candidate>) -> Result<Self, DecisionError> {
        if candidates.is_empty() {
            return Err(DecisionError::InvalidResponse(
                "a distribution has no candidate".into(),
            ));
        }
        if let Some(bad) = candidates
            .iter()
            .find(|c| !(0.0..=1.0).contains(&c.probability))
        {
            return Err(DecisionError::InvalidResponse(format!(
                "candidate {:?} has probability {}, outside 0..=1",
                bad.id, bad.probability
            )));
        }
        candidates.sort_by(|a, b| {
            b.probability
                .total_cmp(&a.probability)
                .then(a.index.cmp(&b.index))
        });
        Ok(Self {
            ranked: candidates,
            confidence: None,
            state_truncated: false,
        })
    }

    /// The best candidate.
    #[must_use]
    pub fn top(&self) -> &Candidate {
        &self.ranked[0]
    }

    /// How far the best candidate leads the second; the best's own
    /// probability when it is alone.
    #[must_use]
    pub fn margin(&self) -> f32 {
        let first = self.ranked[0].probability;
        self.ranked
            .get(1)
            .map_or(first, |second| first - second.probability)
    }

    /// The probability of the candidate named `id`.
    #[must_use]
    pub fn probability_of(&self, id: &str) -> Option<f32> {
        self.ranked
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.probability)
    }
}

/// The answer to one question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Answer {
    Ranked(Distribution),
    /// The model does not answer questions of this kind.
    Unsupported {
        reason: String,
    },
}

impl Answer {
    /// A ranked answer for a choice, from `(option id, probability)` in the
    /// caller's order.
    ///
    /// # Errors
    ///
    /// As [`Distribution::from_candidates`].
    pub fn choice(candidates: &[(&str, f32)]) -> Result<Self, DecisionError> {
        Distribution::from_candidates(candidates).map(Self::Ranked)
    }
}

/// An answer with the question it belongs to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub kind: QuestionKind,
    pub answer: Answer,
}

/// Which model answered, and which bytes of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdentity {
    pub id: String,
    /// Identity of the weights, when the provider reports it: a different
    /// checkpoint under the same id is a different model.
    pub checkpoint: Option<String>,
}

/// The answers to a [`crate::DecisionRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResult {
    pub model: ModelIdentity,
    /// Time the model spent answering.
    pub latency: Duration,
    /// The most input tokens the model holds, when it reports it.
    pub max_input_tokens: Option<u32>,
    /// Some state was cut to fit the model's context.
    pub state_truncated: bool,
    /// The answer came from the provider's cache of identical decisions; no
    /// inference ran for this call.
    #[serde(default)]
    pub cached: bool,
    /// Question kinds the model declined.
    pub unsupported_kinds: Vec<QuestionKind>,
    /// One per question, in request order.
    pub answers: Vec<QuestionAnswer>,
}

impl DecisionResult {
    /// A result of `model` with the given `(question id, kind, answer)`.
    #[must_use]
    pub fn for_answers(model: &str, answers: Vec<(&str, QuestionKind, Answer)>) -> Self {
        Self {
            model: ModelIdentity {
                id: model.to_string(),
                checkpoint: None,
            },
            latency: Duration::ZERO,
            max_input_tokens: None,
            state_truncated: false,
            cached: false,
            unsupported_kinds: Vec::new(),
            answers: answers
                .into_iter()
                .map(|(id, kind, answer)| QuestionAnswer {
                    id: id.to_string(),
                    kind,
                    answer,
                })
                .collect(),
        }
    }

    /// The answer to the question named `id`.
    #[must_use]
    pub fn answer(&self, id: &str) -> Option<&QuestionAnswer> {
        self.answers.iter().find(|a| a.id == id)
    }
}
