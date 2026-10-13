// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The decision providers of the benchmark.
//!
//! Each answers like a model does: a full, valid distribution for every
//! question, in the caller's option order. They differ in what they know:
//!
//! * [`HeuristicProvider`]: words in common with the option, weighted by how
//!   distinctive they are among the offered options. Knows nothing else.
//! * [`RandomProvider`]: a seeded random distribution; the negative control.
//! * [`OracleProvider`]: the labelled-correct option; the upper bound.
//!
//! Questions other than a choice are answered evenly, which the policy
//! abstains from.

mod heuristic;
mod oracle;
mod random;

use std::time::Duration;

use sven_decision::{
    Answer, Choice, DecisionError, DecisionRequest, DecisionResult, Question, QuestionAnswer,
};

pub use heuristic::HeuristicProvider;
pub use oracle::{OracleLabel, OracleProvider};
pub use random::RandomProvider;

/// Builds the result for `request`, asking `weigh` for the relative weight of
/// each option of a choice (any positive scale, in the caller's order).
/// Everything that is not a choice, and a choice with no weight at all, is
/// answered evenly.
pub(crate) fn answer_request(
    model: &str,
    request: &DecisionRequest,
    weigh: impl Fn(&DecisionRequest, &Choice) -> Vec<f64>,
) -> Result<DecisionResult, DecisionError> {
    request.validate()?;
    let mut answers = Vec::with_capacity(request.questions.len());
    for question in &request.questions {
        let (ids, weights): (Vec<String>, Vec<f64>) = match question {
            Question::Choice(choice) => (
                choice.options.iter().map(|o| o.id.clone()).collect(),
                weigh(request, choice),
            ),
            Question::Probability(_) => (vec!["true".into(), "false".into()], vec![1.0, 1.0]),
            Question::Score(score) => (score.levels.clone(), vec![1.0; score.levels.len()]),
        };
        answers.push(QuestionAnswer {
            id: question.id().to_string(),
            kind: question.kind(),
            answer: distribution(&ids, &weights)?,
        });
    }
    let mut result = DecisionResult::for_answers(model, Vec::new());
    result.answers = answers;
    result.latency = Duration::ZERO;
    Ok(result)
}

fn distribution(ids: &[String], weights: &[f64]) -> Result<Answer, DecisionError> {
    let total: f64 = weights.iter().sum();
    let even = 1.0 / ids.len() as f64;
    let candidates: Vec<(&str, f32)> = ids
        .iter()
        .zip(weights)
        .map(|(id, w)| {
            let p = if total > 0.0 { w / total } else { even };
            (id.as_str(), p as f32)
        })
        .collect();
    Answer::choice(&candidates)
}
