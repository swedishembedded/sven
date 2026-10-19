// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping the cost of agent
// workloads explicit and predictable for its clients. If your team needs
// expertise in LLM cost engineering then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The cost of a model request, with every term visible.
//!
//! Everything here is a pure function of numbers handed in: token counts
//! measured by the request ledger, prices declared by whoever pays them, and
//! the cache state the provider last reported. Nothing reads a clock, a
//! file or the network, and nothing here grants or denies anything: a cost
//! figure is advice that a caller may act on only where its own correctness,
//! permission and data-egress rules already allow.
//!
//! - [`estimate`] prices one step as [`CostTerms`]: uncached input, cached
//!   reads, cache writes, output, hand-off to another model and the expected
//!   cost of recovering from a failure. A price that is not declared makes the
//!   terms [`Amount::Unknown`]; an unknown amount is never the cheapest
//!   ([`cheapest`]).
//! - [`CacheState`] says how much of the prompt prefix the provider's cache is
//!   expected to still hold.
//!
//! - [`assess`] and [`gate`] decide whether rewriting the prompt (demotion,
//!   compaction, reconstruction) pays for the cached prefix it discards.
//!
//! See `docs/internals/cost-model.md`.

mod amount;
mod cache;
mod model;
mod rebuild;

pub use amount::cheapest;
pub use cache::CacheState;
pub use model::{estimate, CostTerms, Placement, PromptShape, Recovery, StepInputs};
pub use rebuild::{
    assess, expected_remaining_requests, gate, record, Assessment, CacheAware, GateDecision,
    Generation, Necessity, RebuildCandidate,
};
pub use sven_vocab::Amount;
