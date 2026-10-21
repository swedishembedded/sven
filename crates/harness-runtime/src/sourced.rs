// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A trace in the author's terms.
//!
//! A version 2 workflow lowers to many generated steps, and its plan's source
//! map says which task, role, gate and attempt each one is for. A
//! [`SourceTracker`] walks a trace in order and attaches that to every record
//! it can: the task, the role of the step, the gate it evaluates, the
//! verification attempt, the work item whose body is running, and the result
//! (`passed`, `failed`, `completed`). Records are unchanged otherwise, so a
//! reader that ignores `source` reads the same trace.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use serde::Serialize;
use sven_harness_spec::HarnessPlan;

use crate::run::TraceRecord;

/// What a trace record means in the workflow's own terms.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Source {
    /// The authored task the record belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// What the step does for its task (`work`, `check`, `repair`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The gate or check the step evaluates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    /// The verification attempt: 0 is the first, 1 the one after the first
    /// repair.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The work item whose body the record belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    /// How the thing the record reports ended: `passed`, `failed` or
    /// `completed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

impl Source {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// A trace record with its source, serialized as the record's own fields plus
/// `source`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourcedRecord {
    /// The record as the machine wrote it.
    #[serde(flatten)]
    pub record: TraceRecord,
    /// Where it came from in the document; absent for a version 1 plan and for
    /// records that belong to no authored step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
}

/// Attaches [`Source`] to the records of one run, fed in order.
pub struct SourceTracker<P> {
    plan: P,
    /// The step most recently started: a `Verified` record carries none of
    /// its own.
    current: Option<String>,
    /// The item being worked, by the authored task of its loop.
    items: BTreeMap<String, String>,
}

impl<P: std::borrow::Borrow<HarnessPlan>> SourceTracker<P> {
    /// A tracker for a run of `plan` (a reference or an `Arc`).
    #[must_use]
    pub fn new(plan: P) -> Self {
        Self {
            plan,
            current: None,
            items: BTreeMap::new(),
        }
    }

    /// The next record of the run, with its source.
    pub fn next(&mut self, record: &TraceRecord) -> SourcedRecord {
        if let TraceRecord::StepStarted { step, .. } = record {
            self.current = Some(step.clone());
        }
        let step = match record {
            TraceRecord::Verified { .. } => self.current.as_deref(),
            other => step_of(other),
        };
        let mut source = Source::default();
        if let Some(origin) = step.and_then(|s| self.plan.borrow().source_map.get(s)) {
            source.task = origin.task.clone();
            source.role = Some(origin.role.clone());
            source.gate = origin.check.clone();
            source.attempt = origin.attempt;
        }
        match record {
            TraceRecord::ItemStarted { item, .. } => {
                if let Some(task) = &source.task {
                    self.items.insert(task.clone(), item.clone());
                }
                source.item_id = Some(item.clone());
            }
            TraceRecord::ItemCompleted { item, .. } => {
                if let Some(task) = &source.task {
                    self.items.remove(task);
                }
                source.item_id = Some(item.clone());
                source.result = Some("completed".to_string());
            }
            TraceRecord::ItemFailed { item, .. } => {
                if let Some(task) = &source.task {
                    self.items.remove(task);
                }
                source.item_id = Some(item.clone());
                source.result = Some("failed".to_string());
            }
            _ => {
                source.item_id = source
                    .task
                    .as_ref()
                    .and_then(|t| self.items.get(t))
                    .cloned();
            }
        }
        let verdict = |ok: bool| Some(if ok { "passed" } else { "failed" }.to_string());
        match record {
            TraceRecord::GateEvaluated { passed, .. } => source.result = verdict(*passed),
            TraceRecord::Verified { ok, .. } | TraceRecord::ChildCompleted { ok, .. } => {
                source.result = verdict(*ok);
            }
            _ => {}
        }
        SourcedRecord {
            record: record.clone(),
            source: (!source.is_empty()).then_some(source),
        }
    }
}

/// The step a record is about, when it names one.
fn step_of(record: &TraceRecord) -> Option<&str> {
    match record {
        TraceRecord::StepStarted { step, .. }
        | TraceRecord::StepCompleted { step, .. }
        | TraceRecord::BranchSelected { step, .. }
        | TraceRecord::IterationStarted { step, .. }
        | TraceRecord::ItemStarted { step, .. }
        | TraceRecord::ItemCompleted { step, .. }
        | TraceRecord::ItemFailed { step, .. }
        | TraceRecord::ItemsReconciled { step, .. }
        | TraceRecord::ItemsInvalidated { step, .. }
        | TraceRecord::TurnRetried { step, .. }
        | TraceRecord::GateEvaluated { step, .. }
        | TraceRecord::RetryAttempt { step, .. }
        | TraceRecord::ChildStarted { step, .. }
        | TraceRecord::ChildCompleted { step, .. }
        | TraceRecord::FanOut { step, .. }
        | TraceRecord::Joined { step, .. }
        | TraceRecord::Failed { step, .. } => Some(step),
        TraceRecord::Verified { .. } | TraceRecord::Outcome { .. } => None,
    }
}

/// A whole trace with its sources.
#[must_use]
pub fn sourced_trace(plan: &HarnessPlan, trace: &[TraceRecord]) -> Vec<SourcedRecord> {
    let mut tracker = SourceTracker::new(plan);
    trace.iter().map(|record| tracker.next(record)).collect()
}
