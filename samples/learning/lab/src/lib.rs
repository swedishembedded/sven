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
//! * [`AdapterPath`] - proof that promoted adapters reach the model an arm is
//!   about to measure. Scoring requires one, so measuring the wrong weights is
//!   not a mistake to catch in review.
//! * [`Verdict`] / [`PredicateSet`] - the only source of a "solved". There is
//!   no constructor a sample could use to mark its own work correct.
//! * [`Family`] - a task contract that does not load unless every predicate it
//!   scores says where it came from.
//! * [`Recorder`] - the exact request the agent sent, captured at the wire.
//!   An agent's stored history holds neither the system prompt nor the tool
//!   schemas, so it is not enough to train on.
//! * [`record_from_episode`] - training data from a VERIFIED episode only,
//!   supervising assistant turns and nothing else, and refusing a transcript
//!   with a hole in it.
//!
//! # Making the expensive mistakes impossible rather than noticed
//!
//! Three of these types exist because of failures that were observed while
//! building this harness, and each shares a shape: the run completes, reports
//! a plausible number, and the number is wrong. A warning is a poor defence
//! against that - one of these failures HAD a warning available and it
//! scrolled past in a log - so where it was possible to make the mistake
//! unrepresentable instead, that is what these types do.

mod dataset;
mod endpoint;
mod episode;
mod family;
mod model_id;
mod recorder;
mod score;
mod verdict;

pub use dataset::{record_from_episode, to_jsonl, Excluded, Provenance, Record, RecordMetadata};
pub use endpoint::{AdapterPath, NoAdapterPath};
pub use episode::{baseline_effective, run_verifier, run_witness, Episode, EpisodeError};
pub use family::{Family, FamilyError};
pub use model_id::ServedModel;
pub use recorder::{capture_path, upstream_of, Recorder};
pub use score::{ArmScore, Outcome};
pub use verdict::{PredicateSet, Unevaluated, Verdict};
