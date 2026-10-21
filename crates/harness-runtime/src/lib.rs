// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness machine: executes compiled
//! [`sven-harness-spec`](sven_harness_spec) plans through the HSM kernel.
//!
//! There is no second engine here (ADR 0004). [`HarnessMachine`] is one
//! generic `Machine` over a fixed [`HarnessState`] hierarchy that interprets
//! a [`sven_harness_spec::HarnessPlan`] by emitting the kernel's existing
//! typed `Effect`s and consuming the kernel's existing `Event`s, exactly as
//! the hardcoded machines do. The kernel dispatches, the executors perform
//! I/O, the permission checker gates — all as before; a harness gains
//! nothing at a trust boundary that `sdlc` or `verified-task` does not
//! already have.
//!
//! The state hierarchy is **fixed** (eleven states, whatever the plan's
//! size): the current step id and every step result live in the durable
//! [`RUN_STATE_FACT`] context fact, so adding a step to a harness adds data,
//! not machine states, and a run snapshots and restores like any other
//! kernel machine.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

mod agent_loop;
mod approval;
mod budget;
mod checkpoint;
mod command_gate;
mod coverage;
mod decoding;
mod delegation;
mod error;
mod events;
mod exec;
mod iteration;
mod machine;
mod registry;
mod retry;
mod run;
mod sourced;
mod state;

pub use approval::{HeldCall, HeldGate};
pub use budget::Ceiling;
pub use command_gate::{plan_reads_trajectory, INTERRUPTED_OPERATION};
pub use decoding::harness_thread;
pub use delegation::child_result;
pub use error::HarnessRunError;
pub use events::harness_event;
pub use iteration::{ItemStatus, IterationState, WorkItem};
pub use machine::HarnessMachine;
pub use registry::HarnessRegistry;
pub use run::{
    step_call_id, AgentLoop, ChildRun, Counts, HarnessRunData, HarnessRunState, Inflight,
    ParallelJoin, RetryFrame, StepRecord, TraceRecord, Waiting, OUTCOME_FACT, RUN_STATE_FACT,
};
pub use sourced::{sourced_trace, Source, SourceTracker, SourcedRecord};
pub use state::HarnessState;
