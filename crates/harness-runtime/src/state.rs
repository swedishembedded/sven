// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness machine's fixed state hierarchy.
//!
//! Whatever a plan's size, a run lives in exactly one of these eleven states:
//! the current step id and every step result are data in the
//! [`RUN_STATE_FACT`](crate::RUN_STATE_FACT) context fact, not machine states.
//! The hierarchy is flat — every state sits directly under [`HarnessState::Root`]
//! — which is also what makes every non-root state restorable: the snapshot
//! machinery refuses composite states, and there are none.

/// The harness machine's states.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HarnessState {
    /// The root (fixpoint: `superstate(Root) == Root`). Never an active leaf.
    Root,
    /// The run exists but its entry step has not begun: waiting for the
    /// message that starts it.
    Ready,
    /// Pure steps (assign/branch/retry/...) are being worked off. Entered
    /// after every side-effecting step completes; advances by re-entering
    /// itself with the harness's internal advance signal until the next
    /// side-effecting step or terminal is reached.
    Advancing,
    /// A model turn (predict or agent) is in flight.
    WaitingForModel,
    /// A tool call's result is in flight.
    WaitingForTool,
    /// A verifier verdict is in flight.
    WaitingForVerifier,
    /// A delegated child run is in flight.
    WaitingForChild,
    /// A parallel step's branches are running.
    WaitingForJoin,
    /// The run reached a `return` step. Terminal: nothing moves it.
    Completed,
    /// The run reached a `fail` step or exhausted a bound. Terminal.
    Failed,
    /// The user cancelled the run. Terminal.
    Cancelled,
}

impl HarnessState {
    /// `true` for the three states a finished run rests in. A terminal run
    /// absorbs every further event.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            HarnessState::Completed | HarnessState::Failed | HarnessState::Cancelled
        )
    }
}

/// The `Machine::superstate` of the harness hierarchy: flat, everything
/// under [`HarnessState::Root`].
pub(super) fn superstate_of(state: HarnessState) -> HarnessState {
    match state {
        HarnessState::Root => HarnessState::Root,
        _ => HarnessState::Root,
    }
}

/// The `Machine::all_states` of the harness hierarchy: all eleven, Root
/// included (coverage tooling enumerates the whole set; snapshot restore
/// separately refuses Root because other states live under it).
pub(super) const ALL_STATES: [HarnessState; 11] = [
    HarnessState::Root,
    HarnessState::Ready,
    HarnessState::Advancing,
    HarnessState::WaitingForModel,
    HarnessState::WaitingForTool,
    HarnessState::WaitingForVerifier,
    HarnessState::WaitingForChild,
    HarnessState::WaitingForJoin,
    HarnessState::Completed,
    HarnessState::Failed,
    HarnessState::Cancelled,
];

/// Resolves a snapshot's state label back to a state, refusing the root:
/// a running machine is never resting in it (every dispatch drills into a
/// leaf), and [`Machine`]-based restore treats a composite label as corrupt.
pub(super) fn resolve_label(label: &str) -> Option<HarnessState> {
    ALL_STATES
        .into_iter()
        .filter(|s| *s != HarnessState::Root)
        .find(|s| format!("{s:?}") == label)
}
