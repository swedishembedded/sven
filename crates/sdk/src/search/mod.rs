// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A generic search over candidates: best-of-N and simulated annealing.
//!
//! A search takes an initial candidate, a [`Proposer`] that derives new
//! candidates from old ones, a [`CandidateEnvironment`] that evaluates a
//! candidate in isolation and returns a [`SearchEvaluation`], and a
//! [`SearchBudget`]. The frozen task is whatever the environment was built
//! over; the search never changes it. The dense `utility` guides the search;
//! only the verifier's `terminal` verdict makes a candidate correct, so the
//! report's best verified candidate is empty when the verifier passed
//! nothing, however high the utility got.
//!
//! This is a library beside the HSM, not a machine inside it: evaluating a
//! candidate typically runs a harness or an agent through the SDK (their
//! trajectories are referenced from the trace), but the search itself owns no
//! agent loop, scheduler or model call. Evaluations run one after another;
//! a search is reproducible from its seed when the proposer and environment
//! are.
//!
//! Swedish Embedded AB implements reproducible agent search for its clients.
//! If your team needs expertise in verifier-guided search over agent
//! candidates then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod rng;
mod run;
mod types;

use std::future::Future;

pub use rng::SearchRng;
pub use run::{search, SearchError, Strategy};
pub use sven_vocab::verify::VerifierVerdict;
pub use types::{
    EvalLimits, Proposal, ProposalStep, SearchBudget, SearchEvaluation, SearchMetrics,
    SearchReport, StopReason, TraceEntry, TrajectoryRef,
};

/// Derives a new candidate from a parent.
pub trait Proposer<C>: Send + Sync {
    /// Proposes a candidate near `parent`. Randomness must come from `rng`
    /// for the search to be reproducible from its seed. A failure aborts the
    /// search.
    fn propose(
        &self,
        parent: &C,
        step: &ProposalStep,
        rng: &mut SearchRng,
    ) -> impl Future<Output = Result<Proposal<C>, SearchError>> + Send;
}

/// Evaluates a candidate in an environment of its own.
///
/// An implementation provisions an isolated environment for the candidate
/// (a fresh workspace, a sandboxed run), runs it within `limits`, asks the
/// verifier for the terminal verdict, computes the dense utility and drops the
/// environment. A candidate that crashes or times out is a result (a failed
/// verdict with its floor utility), not an error: an `Err` means the
/// evaluation infrastructure failed and aborts the search.
pub trait CandidateEnvironment<C>: Send + Sync {
    /// Evaluates `candidate` within `limits`.
    fn evaluate(
        &self,
        candidate: &C,
        limits: &EvalLimits,
    ) -> impl Future<Output = Result<SearchEvaluation, SearchError>> + Send;
}
