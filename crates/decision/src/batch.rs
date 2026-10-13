// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Batching independent requests about the same state into one call.
//!
//! A decision model encodes the state once for all the questions of a call,
//! so questions that several decision points ask about the same state are
//! cheaper together than apart. [`decide_batched`] merges them, namespacing
//! every question id with its asker's position, makes one provider call per
//! state and hands each asker back exactly the answers to its own questions.

use crate::{
    DecisionError, DecisionProvider, DecisionRequest, DecisionResult, QuestionAnswer, MAX_QUESTIONS,
};

/// What [`decide_batched`] did.
#[derive(Debug)]
pub struct Batched {
    /// One per input request, in order.
    pub results: Vec<Result<DecisionResult, DecisionError>>,
    /// How many provider calls were made.
    pub calls: u32,
    /// How many questions each call carried.
    pub batch_sizes: Vec<usize>,
}

/// Answers every request, sending questions about one state in one call
/// (split only when they exceed [`MAX_QUESTIONS`]). A call's failure is every
/// member's failure; an invalid request is refused alone and never sent.
pub async fn decide_batched(
    provider: &dyn DecisionProvider,
    requests: &[DecisionRequest],
) -> Batched {
    let mut results: Vec<Option<Result<DecisionResult, DecisionError>>> =
        requests.iter().map(|_| None).collect();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, request) in requests.iter().enumerate() {
        if let Err(error) = request.validate() {
            results[i] = Some(Err(error));
            continue;
        }
        let open = groups.iter_mut().find(|group| {
            requests[group[0]].state == request.state
                && group
                    .iter()
                    .map(|&m| requests[m].questions.len())
                    .sum::<usize>()
                    + request.questions.len()
                    <= MAX_QUESTIONS
        });
        match open {
            Some(group) => group.push(i),
            None => groups.push(vec![i]),
        }
    }

    let mut batch_sizes = Vec::with_capacity(groups.len());
    for group in &groups {
        let merged = merge(requests, group);
        batch_sizes.push(merged.questions.len());
        let outcome = provider.decide(&merged).await;
        for &member in group {
            results[member] = Some(match &outcome {
                Ok(result) => split(result, member, &requests[member]),
                Err(error) => Err(error.clone()),
            });
        }
    }
    Batched {
        results: results
            .into_iter()
            .map(|r| r.expect("every request is either refused or in a group"))
            .collect(),
        calls: u32::try_from(groups.len()).unwrap_or(u32::MAX),
        batch_sizes,
    }
}

fn merge(requests: &[DecisionRequest], group: &[usize]) -> DecisionRequest {
    DecisionRequest {
        state: requests[group[0]].state.clone(),
        questions: group
            .iter()
            .flat_map(|&m| {
                requests[m]
                    .questions
                    .iter()
                    .map(move |q| q.renamed(format!("{m}:{}", q.id())))
            })
            .collect(),
        deadline: group
            .iter()
            .map(|&m| requests[m].deadline)
            .min()
            .unwrap_or_default(),
    }
}

fn split(
    merged: &DecisionResult,
    member: usize,
    request: &DecisionRequest,
) -> Result<DecisionResult, DecisionError> {
    let prefix = format!("{member}:");
    let answers: Vec<QuestionAnswer> = merged
        .answers
        .iter()
        .filter_map(|a| {
            a.id.strip_prefix(&prefix).map(|id| QuestionAnswer {
                id: id.to_string(),
                ..a.clone()
            })
        })
        .collect();
    if let Some(missing) = request
        .questions
        .iter()
        .find(|q| !answers.iter().any(|a| a.id == q.id()))
    {
        return Err(DecisionError::InvalidResponse(format!(
            "no answer to question {:?}",
            missing.id()
        )));
    }
    Ok(DecisionResult {
        answers,
        unsupported_kinds: merged
            .unsupported_kinds
            .iter()
            .copied()
            .filter(|kind| request.questions.iter().any(|q| q.kind() == *kind))
            .collect(),
        ..merged.clone()
    })
}
