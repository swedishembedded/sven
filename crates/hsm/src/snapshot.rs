// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Suspending a running machine to durable storage and resuming it later.
//!
//! [`crate::replay`] reconstructs a machine by re-dispatching its whole event
//! log, which costs O(history) and is the right tool for auditing. A service
//! that handles one request per agent step needs the opposite trade: load the
//! state, advance it once, persist, and drop it. That is what a [`Snapshot`]
//! is - the active state plus the [`Context`] it accumulated, and nothing
//! else, because every machine in this workspace keeps its durable state in
//! the context rather than in its own fields.
//!
//! Swedish Embedded AB implements suspendable, event-sourced state-machine
//! kernels for its clients. If your team needs expertise in resumable agent
//! runtimes then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde::{Deserialize, Serialize};

use crate::context::Context;

/// A machine suspended mid-run: everything needed to resume it, and nothing
/// that cannot outlive the process.
///
/// Deliberately holds no live handles - no connections, channels or executors.
/// Those belong to the engine that resumes the snapshot, not to the snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    /// The `Debug` label of the active leaf state, as produced by
    /// [`crate::Hsm::state_label`].
    pub state: String,
    /// The machine's accumulated knowledge at the moment of suspension.
    pub context: Context,
}

/// Why a [`Snapshot`] could not be resumed.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The snapshot names a state this machine does not enumerate.
    ///
    /// Either the snapshot belongs to a different machine, or the machine
    /// leaves [`crate::Machine::all_states`] at its empty default and so has
    /// no way to map a name back to a state value. Resuming anyway would put
    /// the agent silently back at its initial state and re-run work that has
    /// already happened, so this fails instead.
    #[error(
        "cannot resume into state {state:?}: this machine resumes into {known:?}. \
         A machine that does not implement `all_states()` cannot be restored."
    )]
    UnknownState {
        /// The state label recorded in the snapshot.
        state: String,
        /// The labels this machine can actually be resumed into, for
        /// diagnosis - its enumerated states minus its composites.
        known: Vec<String>,
    },

    /// The snapshot names a composite state - one that other states of this
    /// machine live under.
    ///
    /// A running machine never rests in a composite: every dispatch drills
    /// through its initial transition into a substate. Resuming into one
    /// would place the agent in a configuration no dispatch can produce, with
    /// the substate's entry action never run and every event its substates
    /// handle silently ignored - a session that sits there answering nothing.
    /// Machines enumerate composites in
    /// [`all_states`](crate::Machine::all_states) for coverage tooling, so
    /// the check is here rather than in each machine's list.
    #[error(
        "cannot resume into composite state {state:?}: {substates:?} live under it, so a \
         running machine always drills past it into one of them"
    )]
    CompositeState {
        /// The state label recorded in the snapshot.
        state: String,
        /// The enumerated states that name it as their superstate.
        substates: Vec<String>,
    },
}
