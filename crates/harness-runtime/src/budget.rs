// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A run's budgets and how they hold across the runs it starts.
//!
//! A plan declares the model turns, tool calls and steps one run may spend.
//! A run that delegates starts children, and what a child spends is spent
//! under the parent's limits too: the child is started with a [`Ceiling`] of
//! what the parent had left, its own limits can only be tighter, and when it
//! ends its usage is added to the parent's counters. A tree of runs therefore
//! never spends more than its root declared, however deep it goes.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in bounding what unattended agent
//! work may spend then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sven_harness_spec::HarnessPlan;

use crate::run::HarnessRunState;

/// What a parent run had left of its budgets when it started a child.
/// `None` is no limit from the parent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ceiling {
    /// Model turns the child (and everything below it) may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_turns: Option<u64>,
    /// Tool calls the child (and everything below it) may make.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
}

impl Ceiling {
    /// `true` when the parent set no limit.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.model_turns.is_none() && self.tool_calls.is_none()
    }

    /// The ceiling a spawn descriptor carries.
    #[must_use]
    pub fn to_value(self) -> Value {
        json!({ "model_turns": self.model_turns, "tool_calls": self.tool_calls })
    }

    /// The ceiling a spawn descriptor carried; an absent or malformed one is
    /// no limit.
    #[must_use]
    pub fn from_value(value: Option<&Value>) -> Self {
        let number = |key: &str| value.and_then(|v| v.get(key)).and_then(Value::as_u64);
        Self {
            model_turns: number("model_turns"),
            tool_calls: number("tool_calls"),
        }
    }
}

/// The smaller of two optional limits, where `None` is no limit.
fn tighter(declared: Option<u64>, ceiling: Option<u64>) -> Option<u64> {
    match (declared, ceiling) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// The step budget a harness without a declared `max_steps` runs under.
///
/// Validation already requires limits on every harness whose steps can run
/// away; this only bounds a pure chain against a compiler/retry-accounting
/// bug, where an honest run can never get near it.
const FALLBACK_STEP_BUDGET: u64 = 4096;

impl HarnessRunState {
    /// The effective step budget: the declared `max_steps` or the fallback
    /// runaway guard.
    #[must_use]
    pub fn step_budget(&self, plan: &HarnessPlan) -> u64 {
        plan.limits
            .as_ref()
            .and_then(|l| l.max_steps)
            .unwrap_or(FALLBACK_STEP_BUDGET)
    }

    /// The model-turn budget, if any: the declared one, no larger than what
    /// the parent had left.
    #[must_use]
    pub fn model_turn_budget(&self, plan: &HarnessPlan) -> Option<u64> {
        tighter(
            plan.limits.as_ref().and_then(|l| l.max_model_turns),
            self.ceiling.model_turns,
        )
    }

    /// The tool-call budget, if any: the declared one (or the one the run
    /// started with), no larger than what the parent had left.
    #[must_use]
    pub fn tool_call_budget(&self, plan: &HarnessPlan) -> Option<u64> {
        let declared = plan.limits.as_ref().and_then(|limits| {
            // A limit the run was started with replaces the document's number.
            limits
                .max_tool_calls_input
                .as_deref()
                .and_then(|name| self.inputs.get(name))
                .and_then(Value::as_u64)
                .or(limits.max_tool_calls)
        });
        tighter(declared, self.ceiling.tool_calls)
    }

    /// `true` when the run has spent more model turns than its budget
    /// allows (a run without a declared budget is unbounded).
    #[must_use]
    pub fn exceeds_model_budget(&self, plan: &HarnessPlan) -> bool {
        self.model_turn_budget(plan)
            .is_some_and(|budget| self.counts.model_turns > budget)
    }

    /// `true` when the run has spent more tool calls than its budget
    /// allows (a run without a declared budget is unbounded).
    #[must_use]
    pub fn exceeds_tool_budget(&self, plan: &HarnessPlan) -> bool {
        self.tool_call_budget(plan)
            .is_some_and(|budget| self.counts.tool_calls > budget)
    }

    /// What this run has left for a child to spend.
    #[must_use]
    pub fn remaining(&self, plan: &HarnessPlan) -> Ceiling {
        Ceiling {
            model_turns: self
                .model_turn_budget(plan)
                .map(|budget| budget.saturating_sub(self.counts.model_turns)),
            tool_calls: self
                .tool_call_budget(plan)
                .map(|budget| budget.saturating_sub(self.counts.tool_calls)),
        }
    }

    /// Adds what a finished child spent to this run's counters.
    pub fn absorb_usage(&mut self, usage: &Value) {
        let spent = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
        self.counts.model_turns += spent("model_turns");
        self.counts.tool_calls += spent("tool_calls");
    }
}
