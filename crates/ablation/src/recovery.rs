// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Recovery: what the model was sent when the final question was first put to
//! it, what it was sent after it had fetched what shortened results left out,
//! and how often it fetched.
//!
//! The first number is the harness alone: the final question answered from the
//! context as assembled. The second is the harness with a model that follows a
//! reference it was shown. Their difference is the part of the evidence that
//! only reaches the model if it asks.

use serde::{Deserialize, Serialize};

use crate::evidence::{score_evidence, LoggedRequest, RequestLog};
use crate::task::Task;

/// Critical evidence in the request that first carried the final question
/// and in the request the model answered from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryScore {
    pub critical_total: usize,
    pub visible_before: usize,
    pub visible_after: usize,
    /// Without any fetch by the model.
    pub recall_before: f64,
    /// After the fetches the model made.
    pub recall_after: f64,
    /// `context` expansions the model called while answering the question.
    pub expansions: usize,
}

/// The request in which the last turn's question first reached the model.
fn first_question_request<'a>(task: &Task, log: &'a RequestLog) -> Option<&'a LoggedRequest> {
    let marker = &task.segments.last()?.turns.last()?.marker;
    let root = log.requests.first()?.pid;
    log.requests.iter().find(|r| {
        r.pid == root
            && r.messages
                .iter()
                .any(|m| m.role == "user" && m.text.contains(marker.as_str()))
    })
}

/// Scores recovery on the last segment's `log`; `None` when the task's final
/// question does not recover.
#[must_use]
pub fn score_recovery(task: &Task, log: &RequestLog) -> Option<RecoveryScore> {
    task.segments.last()?.turns.last()?.recover.as_ref()?;
    let first = first_question_request(task, log)?;
    let last = log.final_request()?;
    let before = score_evidence(&task.evidence, first);
    let after = score_evidence(&task.evidence, last);
    let asked = first.messages.len();
    let expansions = last
        .messages
        .iter()
        .skip(asked)
        .filter(|m| m.kind == "tool_call" && m.name.as_deref() == Some("context"))
        .count();
    Some(RecoveryScore {
        critical_total: after.critical_total,
        visible_before: before.critical_visible,
        visible_after: after.critical_visible,
        recall_before: before.critical_recall,
        recall_after: after.critical_recall,
        expansions,
    })
}
