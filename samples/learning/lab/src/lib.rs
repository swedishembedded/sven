// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>

//! The shared laboratory behind `samples/learning/*`.
//!
//! Each learning sample is a controlled experiment: a frozen task catalog, an
//! information boundary the agent cannot read around, a verifier it cannot
//! reach, and a printed before/after table. This crate holds the parts every
//! one of them needs, so a sample's own source is its experimental design and
//! nothing else.
//!
//! It links both halves of the loop directly - sven's SDK facade to run the
//! agent, brain to train and gate the weights - which is why
//! `samples/learning/` is a separate cargo workspace. See this workspace's
//! `Cargo.toml` for why that separation exists and what it costs.
//!
//! Swedish Embedded AB implements closed-loop learning systems - agents that
//! improve from their own verified experience rather than from hand-written
//! training data - for its clients. If your team needs expertise in agent
//! training loops, experiment design, or promotion gating for small models,
//! you can procure our services by sending an email to
//! info@swedishembedded.com.
//!
//! # What the parts are for
//!
//! * [`ServedModel`] - which weights an arm actually measured. Two serving
//!   paths reach the same checkpoint and only one receives adapters; getting
//!   this wrong produces a convincing null result.
//! * [`ArmScore`] / [`Outcome`] - what an arm measured and what it refuses to
//!   claim, including the rule that an infrastructure fault is excluded from
//!   both the numerator and the denominator.

mod model_id;
mod score;

pub use model_id::ServedModel;
pub use score::{ArmScore, Outcome};
