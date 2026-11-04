// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A harness run's verdict, and the per-step execution counts derived from
//! its structured trace.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in measuring what
//! autonomous workflows actually did then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use sven_harness_runtime::TraceRecord;

use serde_json::Value;
use sven_harness_runtime::TraceRecord as Record;

use super::HarnessReplay;
use crate::run::{RunConclusion, Usage};

/// The verdict of one harness run.
#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    /// How the run ended. A run that reached its own `return` or `fail`
    /// ended on [`RunConclusion::Success`]: the run completed, whatever it
    /// decided. [`RunConclusion::Cancelled`] means the user cancelled it;
    /// the verdict is its cancelled failure. [`RunConclusion::Timeout`]
    /// and [`RunConclusion::BudgetExhausted`] mean an operational bound
    /// suspended it before a verdict.
    pub conclusion: RunConclusion,
    /// The plan's verdict: the `return` value, or the `fail` code and
    /// message; the cancelled failure for a cancelled run. `None` - with
    /// [`super::HarnessRun::checkpoint`] - when a bound suspended the run before
    /// it reached one.
    pub result: Option<Result<Value, HarnessFailure>>,
    /// What the run used, children included.
    pub usage: Usage,
    /// The structured execution trace: what the run did, in order.
    pub trace: Vec<Record>,
    /// How many step executions the run made.
    pub steps: u64,
}

/// A harness run's own failure: the code a `fail` step (or a bound the
/// plan itself exceeded) reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessFailure {
    /// The failure code the plan declared.
    pub code: String,
    /// What the failure means, for a person reading the outcome.
    pub message: String,
}

/// How many times each step started, by step id. A `retry` body that ran
/// twice counts twice; a step that never started is absent.
fn step_counts(trace: &[TraceRecord]) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for record in trace {
        if let TraceRecord::StepStarted { step, .. } = record {
            *counts.entry(step.clone()).or_insert(0) += 1;
        }
    }
    counts
}

impl HarnessOutcome {
    /// How many times each step started, by step id, counted from the
    /// trace. A resumed run's trace includes its first episode's records.
    #[must_use]
    pub fn step_counts(&self) -> BTreeMap<String, u64> {
        step_counts(&self.trace)
    }
}

impl HarnessReplay {
    /// As [`HarnessOutcome::step_counts`], over the replayed trace.
    #[must_use]
    pub fn step_counts(&self) -> BTreeMap<String, u64> {
        step_counts(&self.trace)
    }
}
