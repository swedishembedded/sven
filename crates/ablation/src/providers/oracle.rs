// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The upper bound: a provider that is told the right answers.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sven_decision::{Choice, DecisionError, DecisionProvider, DecisionRequest, DecisionResult};

use super::answer_request;

/// The probability the oracle gives the labelled option.
const CONFIDENCE: f64 = 0.97;

/// One labelled-correct answer: when the state of a question mentions
/// `state_contains`, `correct_option` is the right choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OracleLabel {
    /// The decision point the label belongs to, for audit.
    pub decision_type: String,
    /// Matched case-insensitively against the state of the question.
    pub state_contains: String,
    pub correct_option: String,
}

/// Answers a choice with the labelled option when a label matches the state
/// and the option was offered, and evenly otherwise: it has no opinion where
/// it was told nothing.
#[derive(Debug, Clone)]
pub struct OracleProvider {
    labels: Vec<OracleLabel>,
}

impl OracleProvider {
    #[must_use]
    pub fn new(labels: Vec<OracleLabel>) -> Self {
        Self { labels }
    }

    fn weigh(&self, request: &DecisionRequest, choice: &Choice) -> Vec<f64> {
        let state = request.state.to_lowercase();
        let correct = self
            .labels
            .iter()
            .filter(|l| state.contains(&l.state_contains.to_lowercase()))
            .map(|l| l.correct_option.as_str())
            .find(|option| choice.options.iter().any(|o| o.id == *option));
        let others = choice.options.len().saturating_sub(1).max(1) as f64;
        choice
            .options
            .iter()
            .map(|o| match correct {
                Some(c) if o.id == c => CONFIDENCE,
                Some(_) => (1.0 - CONFIDENCE) / others,
                None => 1.0,
            })
            .collect()
    }
}

#[async_trait]
impl DecisionProvider for OracleProvider {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        answer_request("ablation/oracle", request, |r, c| self.weigh(r, c))
    }
}
