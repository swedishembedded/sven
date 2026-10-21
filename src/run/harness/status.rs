// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Where a suspended run rests, read from its checkpoint and the document.
//!
//! A checkpoint carries the run's whole state. This reads it without running
//! anything: the step the run is at (and which task, check and attempt that
//! is when the document is a workflow), what has completed, the counters, the
//! work items of a loop, and whether the document still matches the program
//! the run was checkpointed under.
//!
//! Swedish Embedded AB implements reviewable, declarative agent workflows for
//! its clients. If your team needs expertise in explaining what a long-running
//! agent job does before it runs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use serde::Serialize;
use serde_json::Value;
use sven_harness_runtime::{HarnessRunState, ItemStatus, TraceRecord, RUN_STATE_FACT};

use super::error::HarnessError;
use super::plan_view::{primitive_word, CompiledHarness};

/// The step a run rests at, in the author's terms when the plan has them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct CurrentStep {
    /// The step id.
    pub step: String,
    /// Its primitive.
    pub primitive: String,
    /// The authored task it belongs to (workflows only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// What it does for the task (workflows only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// The check it belongs to, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The attempt of the task's verification it belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

/// The run's counters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct StatusCounts {
    /// Steps processed.
    pub steps: u64,
    /// Model turns emitted.
    pub model_turns: u64,
    /// Tool calls emitted.
    pub tool_calls: u64,
    /// Child runs spawned.
    pub children: u64,
}

/// A `for_each` loop's progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct LoopProgress {
    /// The iterate step.
    pub step: String,
    /// Items frozen.
    pub total: usize,
    /// Items completed.
    pub completed: usize,
    /// The item whose body was running, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
}

/// Where a suspended run rests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct StatusReport {
    /// The harness's name.
    pub harness: String,
    /// The digest of the plan the document compiles to now.
    pub plan_digest: String,
    /// The run's id.
    pub run_id: String,
    /// The machine's state label at the suspension.
    pub state: String,
    /// Whether `resume` would accept this document with the checkpoint.
    pub resumable: bool,
    /// Why not, when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The step the run rests at.
    pub current: CurrentStep,
    /// What the run was waiting on when it stopped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<String>,
    /// The counters.
    pub counts: StatusCounts,
    /// Steps that completed, in order.
    pub steps_completed: Vec<String>,
    /// Progress of each `for_each` loop that has begun.
    pub loops: Vec<LoopProgress>,
}

/// Reads `checkpoint` (the JSON `run --checkpoint-out` wrote) against the
/// compiled `harness`.
///
/// # Errors
///
/// [`HarnessError::Harness`] when the value is not a harness checkpoint.
pub(super) fn status(
    harness: &CompiledHarness,
    checkpoint: &Value,
) -> Result<StatusReport, HarnessError> {
    let bad = |what: &str| HarnessError::Harness(format!("not a harness checkpoint: {what}"));
    let text = |key: &str| checkpoint.get(key).and_then(Value::as_str);
    let checkpoint_digest = text("plan_digest").ok_or_else(|| bad("no plan_digest"))?;
    let run_id = text("run_id").ok_or_else(|| bad("no run_id"))?;
    let snapshot = checkpoint
        .get("snapshot")
        .ok_or_else(|| bad("no snapshot"))?;
    let state_label = snapshot
        .get("state")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("no machine state"))?;
    let run: HarnessRunState = snapshot
        .pointer("/context/facts")
        .and_then(|facts| facts.get(RUN_STATE_FACT))
        .cloned()
        .ok_or_else(|| bad("no run state"))
        .and_then(|value| {
            serde_json::from_value(value).map_err(|e| bad(&format!("unreadable run state ({e})")))
        })?;

    let plan = &harness.plan;
    let bundle_digest = checkpoint.get("bundle_digest").and_then(Value::as_str);
    let reason = if checkpoint_digest != plan.digest {
        Some(format!(
            "the document compiles to plan digest {} but the run was checkpointed under {checkpoint_digest}",
            plan.digest
        ))
    } else if bundle_digest != harness.bundle_digest.as_deref() {
        Some("the bundle digest differs from the one the run pinned".to_string())
    } else {
        None
    };
    let origin = plan.source_map.get(&run.current);
    Ok(StatusReport {
        harness: plan.name.clone(),
        plan_digest: plan.digest.clone(),
        run_id: run_id.to_string(),
        state: state_label.to_string(),
        resumable: reason.is_none(),
        reason,
        current: CurrentStep {
            primitive: plan
                .step(&run.current)
                .map(|s| primitive_word(&s.primitive))
                .unwrap_or_default(),
            task: origin.and_then(|o| o.task.clone()),
            role: origin.map(|o| o.role.clone()),
            check: origin.and_then(|o| o.check.clone()),
            attempt: origin.and_then(|o| o.attempt),
            step: run.current.clone(),
        },
        awaiting: run.awaiting.map(|w| format!("{w:?}")),
        counts: StatusCounts {
            steps: run.counts.steps,
            model_turns: run.counts.model_turns,
            tool_calls: run.counts.tool_calls,
            children: run.counts.children,
        },
        steps_completed: run
            .trace
            .iter()
            .filter_map(|record| match record {
                TraceRecord::StepCompleted { step, .. } => Some(step.clone()),
                _ => None,
            })
            .collect(),
        loops: run
            .iterations
            .iter()
            .filter(|(_, state)| !state.items.is_empty())
            .map(|(step, state)| LoopProgress {
                step: step.clone(),
                total: state.items.len(),
                completed: state
                    .items
                    .iter()
                    .filter(|item| item.status == ItemStatus::Completed)
                    .count(),
                active: state
                    .active
                    .and_then(|index| state.items.get(index))
                    .map(|item| item.id.clone()),
            })
            .collect(),
    })
}
