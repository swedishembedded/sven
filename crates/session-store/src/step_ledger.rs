// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The token ledger as it lives in a saved trajectory.
//!
//! Each model request's [`RequestBreakdown`] is stored in the `extra` of the
//! agent step it belongs to, under [`STEP_METRICS_EXTRA_KEY`], inside
//! `TraceStep.metrics` (ATIF `StepMetrics`). A step can cover several
//! requests (tool results fold into the same turn-shaped step as the next
//! reply), so the value is an array in request order. The provider-reported
//! counters of the same requests are also summed into `StepMetrics`'s own
//! `prompt_tokens`, `completion_tokens` and `cached_tokens`, which keep their
//! ATIF meaning: the prompt total includes cached tokens.
//!
//! [`summarize`] is the pure aggregator over a whole trajectory, subagents
//! included. Trajectories written before the ledger existed carry no records
//! and summarize to zero requests.

use anyhow::{Context, Result};
use atif::{StepMetrics, TraceStep, Trajectory};
use serde_json::{Map, Value};
use sven_vocab::{
    ContextRecovery, DecisionTraceRecord, LedgerSummary, RequestBreakdown, ToolActivity,
    STEP_DECISIONS_EXTRA_KEY, STEP_METRICS_EXTRA_KEY,
};

/// Add one request's record to a step's metrics, creating them if needed.
pub fn record_request(metrics: &mut Option<StepMetrics>, request: &RequestBreakdown) {
    let metrics = metrics.get_or_insert_with(StepMetrics::default);
    if let Some(reported) = request.reported {
        let add = |slot: &mut Option<u64>, n: u32| *slot = Some(slot.unwrap_or(0) + u64::from(n));
        add(&mut metrics.prompt_tokens, reported.logical_prompt());
        add(&mut metrics.completion_tokens, reported.output);
        add(&mut metrics.cached_tokens, reported.cache_read);
    }
    let extra = metrics
        .extra
        .get_or_insert_with(|| Value::Object(Map::new()));
    let Some(object) = extra.as_object_mut() else {
        return;
    };
    // The decision traces live beside the ledger record, not inside it: the
    // ledger stays the token account it always was.
    let mut ledger_record = request.clone();
    let traces = std::mem::take(&mut ledger_record.decision_trace);
    let records = object
        .entry(STEP_METRICS_EXTRA_KEY)
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(records) = records.as_array_mut() {
        records.push(serde_json::to_value(&ledger_record).expect("a breakdown always serializes"));
    }
    if !traces.is_empty() {
        let stored = object
            .entry(STEP_DECISIONS_EXTRA_KEY)
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(stored) = stored.as_array_mut() {
            stored.extend(
                traces
                    .iter()
                    .map(|t| serde_json::to_value(t).expect("a trace record always serializes")),
            );
        }
    }
}

/// The decision traces recorded in one step, in the order they happened.
///
/// # Errors
///
/// Fails when a record is present but is not a [`DecisionTraceRecord`].
pub fn decisions_in_step(step: &TraceStep) -> Result<Vec<DecisionTraceRecord>> {
    step.metrics
        .as_ref()
        .and_then(|m| m.extra.as_ref())
        .and_then(|extra| extra.get(STEP_DECISIONS_EXTRA_KEY))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|record| {
            serde_json::from_value(record.clone())
                .with_context(|| format!("step {}: unreadable decision record", step.step_id))
        })
        .collect()
}

/// The requests recorded in one step, in order. A step without ledger
/// records yields none.
///
/// # Errors
///
/// Fails when a record is present but is not a [`RequestBreakdown`].
pub fn requests_in_step(step: &TraceStep) -> Result<Vec<RequestBreakdown>> {
    let records = step
        .metrics
        .as_ref()
        .and_then(|m| m.extra.as_ref())
        .and_then(|extra| extra.get(STEP_METRICS_EXTRA_KEY))
        .and_then(Value::as_array);
    records
        .into_iter()
        .flatten()
        .map(|record| {
            serde_json::from_value(record.clone())
                .with_context(|| format!("step {}: unreadable ledger record", step.step_id))
        })
        .collect()
}

/// The larger of two snapshots of the same session's expansion counters.
/// Every figure only grows as the history grows, so the request with the most
/// expansions carries the largest of each.
fn largest_recovery(a: ContextRecovery, b: ContextRecovery) -> ContextRecovery {
    ContextRecovery {
        expansions: a.expansions.max(b.expansions),
        stale_expansions: a.stale_expansions.max(b.stale_expansions),
        expanded_bytes: a.expanded_bytes.max(b.expanded_bytes),
        expanded_tokens: a.expanded_tokens.max(b.expanded_tokens),
    }
}

/// Sum the ledger of `trajectory` and of every subagent trajectory embedded
/// in it, at any depth.
///
/// Tool counters are snapshots of the history each request carried, so a
/// trajectory contributes the largest snapshot among its requests.
///
/// # Errors
///
/// Fails when any step holds an unreadable ledger record.
pub fn summarize(trajectory: &Trajectory) -> Result<LedgerSummary> {
    let mut summary = LedgerSummary {
        trajectories: 1,
        ..LedgerSummary::default()
    };
    let mut activity = ToolActivity::default();
    let mut recovery = ContextRecovery::default();
    for step in &trajectory.steps {
        for request in requests_in_step(step)? {
            summary.add_request(&request);
            activity.calls = activity.calls.max(request.tool_activity.calls);
            activity.duplicate_calls = activity
                .duplicate_calls
                .max(request.tool_activity.duplicate_calls);
            recovery = largest_recovery(recovery, request.context_recovery);
        }
    }
    summary.add_tool_activity(activity);
    summary.add_context_recovery(recovery);
    for child in trajectory.subagent_trajectories.iter().flatten() {
        summary.merge(&summarize(child)?);
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use atif::{StepOrigin, Trajectory};
    use sven_vocab::ReportedUsage;

    fn request(user: u32, input: u32, calls: u32, duplicates: u32) -> RequestBreakdown {
        let mut b = RequestBreakdown {
            user,
            estimated_total: user,
            tool_activity: ToolActivity {
                calls,
                duplicate_calls: duplicates,
                ..ToolActivity::default()
            },
            ..RequestBreakdown::default()
        };
        b.reconcile(ReportedUsage {
            input,
            output: 3,
            cache_read: 2,
            cache_write: 0,
        });
        b
    }

    fn trajectory(requests: &[RequestBreakdown]) -> Trajectory {
        let mut t = Trajectory::new("ATIF-v1.7", crate::trace_session::default_agent_profile());
        for (i, r) in requests.iter().enumerate() {
            let mut step = TraceStep::new(i as u64 + 1, StepOrigin::Agent, "reply");
            record_request(&mut step.metrics, r);
            t.steps.push(step);
        }
        t
    }

    #[test]
    fn a_request_survives_the_trajectory_file_round_trip() {
        let t = trajectory(&[request(10, 20, 1, 0)]);
        let json = serde_json::to_string(&t).unwrap();
        let back: Trajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(
            requests_in_step(&back.steps[0]).unwrap(),
            vec![request(10, 20, 1, 0)]
        );
        let m = back.steps[0].metrics.as_ref().unwrap();
        assert_eq!(
            (m.prompt_tokens, m.completion_tokens, m.cached_tokens),
            (Some(22), Some(3), Some(2))
        );
    }

    #[test]
    fn a_trajectory_without_ledger_records_still_reads_and_summarizes_to_nothing() {
        let old = r#"{"schema_version":"ATIF-v1.7","agent":{"name":"sven","version":"1"},
            "steps":[{"step_id":1,"source":"agent","message":"hi",
            "metrics":{"prompt_tokens":5,"completion_tokens":1}}]}"#;
        let t: Trajectory = serde_json::from_str(old).unwrap();
        let s = summarize(&t).unwrap();
        assert_eq!((s.requests, s.trajectories), (0, 1));
    }

    #[test]
    fn the_summary_adds_subagents_and_takes_each_trajectorys_largest_tool_snapshot() {
        let mut parent = trajectory(&[request(10, 20, 1, 0), request(10, 30, 3, 1)]);
        let child = trajectory(&[request(4, 6, 2, 2)]);
        let grandchild = trajectory(&[request(1, 1, 5, 0)]);
        let mut child = child;
        child.subagent_trajectories = Some(vec![grandchild]);
        parent.subagent_trajectories = Some(vec![child]);

        let s = summarize(&parent).unwrap();
        assert_eq!((s.requests, s.trajectories), (4, 3));
        assert_eq!(s.user, 25);
        assert_eq!(s.input, 57);
        assert_eq!((s.tool_calls, s.duplicate_tool_calls), (3 + 2 + 5, 1 + 2));
    }

    #[test]
    fn decision_traces_are_stored_beside_the_ledger_and_not_inside_it() {
        let mut r = request(10, 20, 1, 0);
        r.decision_trace = vec![
            DecisionTraceRecord {
                policy: "tool_routing".into(),
                inputs_digest: "ab12".into(),
                verdict: "accept".into(),
                selected: Some("edit_file".into()),
                applied: true,
                ..DecisionTraceRecord::default()
            },
            DecisionTraceRecord {
                policy: "tool_routing".into(),
                inputs_digest: "cd34".into(),
                verdict: "abstain".into(),
                abstain_reason: Some("near_tie".into()),
                ..DecisionTraceRecord::default()
            },
        ];
        let mut metrics = None;
        record_request(&mut metrics, &r);
        record_request(&mut metrics, &request(1, 1, 0, 0));
        let step = TraceStep {
            metrics,
            ..TraceStep::new(1, StepOrigin::Agent, "reply")
        };
        let extra = step.metrics.as_ref().unwrap().extra.as_ref().unwrap();
        assert!(extra[STEP_METRICS_EXTRA_KEY][0]
            .get("decision_trace")
            .is_none());
        assert_eq!(extra[STEP_DECISIONS_EXTRA_KEY].as_array().unwrap().len(), 2);

        let traces = decisions_in_step(&step).unwrap();
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].selected.as_deref(), Some("edit_file"));
        assert_eq!(traces[1].abstain_reason.as_deref(), Some("near_tie"));
        assert_eq!(requests_in_step(&step).unwrap().len(), 2);
    }

    #[test]
    fn a_trajectory_contributes_the_largest_expansion_snapshot_not_their_sum() {
        let at = |expansions: u32, tokens: u64| {
            let mut r = request(1, 1, 0, 0);
            r.context_recovery = ContextRecovery {
                expansions,
                stale_expansions: expansions.saturating_sub(1),
                expanded_bytes: tokens * 4,
                expanded_tokens: tokens,
            };
            r
        };
        let t = trajectory(&[at(1, 10), at(3, 50), at(3, 50)]);
        let s = summarize(&t).unwrap();
        assert_eq!(
            (
                s.expansions,
                s.stale_expansions,
                s.expanded_bytes,
                s.expanded_tokens
            ),
            (3, 2, 200, 50)
        );
    }

    #[test]
    fn an_unreadable_record_is_an_error_not_a_silent_zero() {
        let mut t = trajectory(&[request(1, 1, 0, 0)]);
        let m = t.steps[0].metrics.as_mut().unwrap();
        m.extra = Some(serde_json::json!({ STEP_METRICS_EXTRA_KEY: [{"user": "many"}] }));
        assert!(summarize(&t).is_err());
    }
}
