// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a harness run is assembled from, and how it is driven: the tools only
//! the harness's own steps call, the event that answers the operation a
//! suspension interrupted, cancellation, checkpoints, and the trace a run
//! publishes as it goes.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use sven_harness_runtime::{harness_event, HarnessRunState, TraceRecord, Waiting};
use sven_hsm::{Context, EffectKind, Event, Snapshot};
use sven_vocab::SessionEvent;
use tokio::sync::broadcast;

use super::run::HarnessFailure;
use super::HarnessCheckpoint;

/// How long a stopped run is given to report what it had before it is
/// left - the same grace a mode session's wind-down gets.
pub(super) const WIND_DOWN: Duration = Duration::from_secs(5);

/// The event that settles what a suspension interrupted, from what the
/// run was waiting on.
///
/// A resumed run must not re-execute the parked step (the tick rule
/// forbids a second child for the same `delegate`); it must answer the
/// operation that was in flight when the suspension hit, and answer it as
/// a failure, so the step's declared retry policy decides what becomes of
/// it. Which event answers depends on the waiter: the tool arm only
/// accepts the result of the exact call it parked on, so it needs the
/// call id the run recorded; the model, verifier, child and join arms
/// settle on a failed effect.
pub(super) fn interrupted_failure(state: &HarnessRunState) -> Option<Event> {
    const SUSPENDED: &str = sven_harness_runtime::INTERRUPTED_OPERATION;
    let inflight = state.inflight.as_ref()?;
    match state.awaiting? {
        Waiting::Model => Some(Event::LlmFailed {
            error: SUSPENDED.to_string(),
        }),
        Waiting::Tool => Some(Event::ToolFailed {
            call_id: inflight.call_id?,
            error: SUSPENDED.to_string(),
        }),
        Waiting::Verifier => Some(Event::EffectFailed {
            kind: EffectKind::Verify,
            error: SUSPENDED.to_string(),
        }),
        Waiting::Child => Some(Event::EffectFailed {
            kind: EffectKind::InstantiateSubmachine,
            error: SUSPENDED.to_string(),
        }),
    }
}

/// The tools only a harness's own steps call - the gate command runner and
/// the snapshot reader - followed by the host's own tools. They exist for every
/// harness run whatever toolset the engine has, confined to the project root;
/// a host tool of the same name (a test fixture, say) is registered later and
/// so replaces them. Without a project root there is nowhere to run a gate,
/// and a gate then fails closed because the tool is missing.
pub(super) fn harness_tools(
    root: Option<&std::path::Path>,
    host: Vec<Arc<dyn sven_tool_api::Tool>>,
    trajectory: sven_tools_exec::TrajectoryHook,
) -> Vec<Arc<dyn sven_tool_api::Tool>> {
    let mut tools: Vec<Arc<dyn sven_tool_api::Tool>> = Vec::new();
    if let Some(root) = root {
        if let Ok(gate) = sven_tools_exec::GateCommandTool::new(root) {
            tools.push(Arc::new(gate.with_trajectory_hook(trajectory)));
        }
        if let Ok(snapshot) = sven_tools_fs::ReadJsonSnapshotTool::new(root) {
            tools.push(Arc::new(snapshot));
        }
        if let Ok(digest) = sven_tools_fs::DigestFilesTool::new(root) {
            tools.push(Arc::new(digest));
        }
    }
    // The variable tools are answered by the machine; registering them gives
    // the model their definitions when a step offers them.
    for tool in sven_tools_agent::HarnessVarTool::all() {
        tools.push(Arc::new(tool));
    }
    tools.extend(host);
    tools
}

/// Cancels a run: the machine is told the user cancelled, settles into its
/// own terminal cancelled state - propagating the cancellation through the
/// children it started - and reports a cancelled verdict. Nothing is left
/// to resume.
pub(super) async fn settle_cancel(
    handle: &sven_bootstrap::RuntimeHandle,
    abort_slot: &tokio::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    wind_down: std::pin::Pin<&mut tokio::time::Sleep>,
) {
    if let Some(abort) = abort_slot.lock().await.take() {
        let _ = abort.send(());
    }
    let _ = handle.cancel().await;
    wind_down.reset(tokio::time::Instant::now() + WIND_DOWN);
}

/// The checkpoint of a run resting at `snapshot`. The program it ran under
/// is the run state's recorded digest, or `plan_digest` when the run was
/// stopped before its first step recorded one.
pub(super) fn checkpoint_at(
    snapshot: Snapshot,
    plan_digest: &str,
    run_id: &str,
) -> HarnessCheckpoint {
    let state = HarnessRunState::load(&snapshot.context);
    HarnessCheckpoint {
        plan_digest: state.plan_digest.unwrap_or_else(|| plan_digest.to_string()),
        bundle_digest: state.bundle_digest,
        run_id: run_id.to_string(),
        snapshot,
    }
}

/// Publishes the trace records `ctx` gained since the last publish: each
/// as a raw record on the trace stream and as its session event on the
/// event stream, so one run's progress has one form everywhere. `observed`
/// carries when the previous record was seen; a step's duration is the gap
/// between observations - approximately what the step cost, zero when the
/// run is observed in a burst. Says which loops completed an item among
/// the records published.
pub(super) fn publish_trace(
    ctx: &Context,
    emitted: &mut usize,
    trace_tx: &broadcast::Sender<TraceRecord>,
    events: &broadcast::Sender<SessionEvent>,
    run_id: &str,
    observed: &mut Option<tokio::time::Instant>,
) -> Vec<String> {
    let state = HarnessRunState::load(ctx);
    let mut completed_loops = Vec::new();
    for record in state.trace.iter().skip(*emitted) {
        if let Some(step) = finished_item_loop(record) {
            completed_loops.push(step.to_string());
        }
        let duration_ms = observed
            .replace(tokio::time::Instant::now())
            .map_or(0, |previous| previous.elapsed().as_millis() as u64);
        let _ = trace_tx.send(record.clone());
        let _ = events.send(SessionEvent::Harness {
            run_id: run_id.to_string(),
            event: harness_event(record, duration_ms),
        });
    }
    *emitted = state.trace.len();
    completed_loops
}

/// The verdict a finished run reported, read off its terminal state and
/// its outcome fact. `None` when the run has no verdict: a bound stopped
/// it first.
pub(super) fn outcome_of(
    state_label: &str,
    outcome: Option<&Value>,
) -> Option<Result<Value, HarnessFailure>> {
    match state_label {
        "Completed" => Some(Ok(outcome.cloned().unwrap_or(Value::Null))),
        "Failed" => {
            let code = outcome
                .and_then(|o| o.get("code"))
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_string();
            let message = outcome
                .and_then(|o| o.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the run failed")
                .to_string();
            Some(Err(HarnessFailure { code, message }))
        }
        "Cancelled" => Some(Err(HarnessFailure {
            code: "cancelled".to_string(),
            message: "the user cancelled the run".to_string(),
        })),
        _ => None,
    }
}

/// The loop a record says finished one of its items (completed or failed):
/// the moment a host is handed an item checkpoint.
pub(super) fn finished_item_loop(record: &sven_harness_runtime::TraceRecord) -> Option<&str> {
    match record {
        sven_harness_runtime::TraceRecord::ItemCompleted { step, .. }
        | sven_harness_runtime::TraceRecord::ItemFailed { step, .. } => Some(step),
        _ => None,
    }
}
