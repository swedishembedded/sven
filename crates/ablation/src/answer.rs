// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Verifying a real model's run from what its final answer cites.
//!
//! A scripted run is scored from what the model was sent. A real model decides
//! for itself what to read and say, so its run is verified by its answer: the
//! tasks ask the model to cite the evidence, and every task labels which
//! evidence is critical. An answer that cites all of it succeeded; citing a
//! distractor is counted, not failed. This verifies a task's answer, not that
//! the run was efficient: cost per verified success is computed from this and
//! the token ledger.

use serde::{Deserialize, Serialize};

use crate::task::Task;

/// What a final answer cited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerScore {
    pub critical_total: usize,
    pub critical_cited: usize,
    pub distractors_cited: usize,
    /// Every critical piece of evidence was cited.
    pub success: bool,
}

/// Scores `reply` against the labelled evidence of `task`.
#[must_use]
pub fn score_answer(task: &Task, reply: &str) -> AnswerScore {
    let critical_total = task.critical().count();
    let critical_cited = task
        .critical()
        .filter(|e| reply.contains(&e.marker))
        .count();
    let distractors_cited = task
        .distractors()
        .filter(|e| reply.contains(&e.marker))
        .count();
    AnswerScore {
        critical_total,
        critical_cited,
        distractors_cited,
        success: critical_total > 0 && critical_cited == critical_total,
    }
}
