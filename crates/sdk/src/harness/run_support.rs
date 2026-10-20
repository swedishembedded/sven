// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a harness run is assembled from, and how a resumed one picks up: the
//! tools only the harness's own steps call, and the event that answers the
//! operation a suspension interrupted.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use sven_harness_runtime::{HarnessRunState, Waiting};
use sven_hsm::{EffectKind, Event};

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
        Waiting::Child | Waiting::Join => Some(Event::EffectFailed {
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
    tools.extend(host);
    tools
}
