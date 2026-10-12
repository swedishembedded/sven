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

mod error;
mod exec;
mod machine;
mod run;
mod state;

pub use error::HarnessRunError;
pub use machine::HarnessMachine;
pub use run::{
    step_call_id, AgentLoop, Counts, HarnessRunData, HarnessRunState, Inflight, RetryFrame,
    StepRecord, Waiting, RUN_STATE_FACT,
};
pub use state::HarnessState;
