// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Per-step execution counts, derived from a run's structured trace.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in measuring what
//! autonomous workflows actually did then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use sven_harness_runtime::TraceRecord;

use super::run::HarnessOutcome;
use super::HarnessReplay;

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
