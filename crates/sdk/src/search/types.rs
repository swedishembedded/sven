// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The vocabulary of a search: what an evaluation returns, what bounds it,
//! and what a finished search reports.
//!
//! Swedish Embedded AB implements reproducible agent search for its clients.
//! If your team needs expertise in verifier-guided search over agent
//! candidates then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sven_vocab::verify::VerifierVerdict;

/// A reference to a sven trajectory a candidate's proposal or evaluation
/// produced, by the identifiers the embedding application stored it under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrajectoryRef {
    /// The run's id (as `HarnessOutcome`/`RunOutcome` report it).
    pub run_id: String,
    /// Where the trajectory lives, when the application stored it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
}

/// What evaluating one candidate in its environment returned.
///
/// `terminal` is the verifier's word and the only thing that makes a
/// candidate correct. `utility` is dense guidance for the search (partial
/// credit); a high utility with no `Passed` verdict is not success.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchEvaluation {
    /// The terminal verifier verdict; `None` when no verifier ran (budget
    /// ran out, the candidate crashed before verification).
    pub terminal: Option<VerifierVerdict>,
    /// A finite score, higher is better, used to rank and to accept moves.
    pub utility: f64,
    /// Named diagnostic measurements (tests passed, lines changed, ...).
    #[serde(default)]
    pub signals: BTreeMap<String, f64>,
    /// Model tokens the evaluation spent.
    #[serde(default)]
    pub tokens: u64,
    /// The sven trajectories the evaluation produced.
    #[serde(default)]
    pub trajectories: Vec<TrajectoryRef>,
}

impl SearchEvaluation {
    /// Whether the verifier accepted the candidate. Partial credit is not.
    #[must_use]
    pub fn verified(&self) -> bool {
        matches!(self.terminal, Some(VerifierVerdict::Passed))
    }
}

/// A new candidate and what proposing it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal<C> {
    /// The candidate.
    pub candidate: C,
    /// Model tokens the proposal spent (zero for a mechanical mutation).
    pub tokens: u64,
    /// The trajectories the proposal produced.
    pub trajectories: Vec<TrajectoryRef>,
}

impl<C> Proposal<C> {
    /// A proposal that cost nothing and left no trajectory.
    #[must_use]
    pub fn free(candidate: C) -> Self {
        Self {
            candidate,
            tokens: 0,
            trajectories: Vec::new(),
        }
    }
}

/// Where in the search a proposal is made.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProposalStep {
    /// Zero-based index of the evaluation this proposal feeds.
    pub index: u64,
    /// The annealing temperature at this step; `None` for best-of-N.
    pub temperature: Option<f64>,
}

/// The bounds a search runs under. `max_evaluations` is required: a search
/// without a bound is not a search facility.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchBudget {
    /// The most candidates evaluated (the initial candidate included).
    pub max_evaluations: u64,
    /// The most model tokens spent on proposals and evaluations together.
    pub max_tokens: Option<u64>,
    /// The longest wall-clock time the search may take.
    pub max_wall: Option<Duration>,
    /// End the search as soon as a candidate is verified. Turn off to keep
    /// spending the budget (for diversity or best-utility studies).
    pub stop_on_verified: bool,
}

/// What an environment may spend on one evaluation: what is left of the
/// search's budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvalLimits {
    /// Tokens left, when the search is token-bounded.
    pub tokens_remaining: Option<u64>,
    /// Wall-clock time left, when the search is time-bounded.
    pub wall_remaining: Option<Duration>,
}

/// Why a search ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// A candidate was verified and the budget asked to stop on it.
    Verified,
    /// The evaluation count was spent.
    Evaluations,
    /// The token budget was spent.
    Tokens,
    /// The wall-clock budget was spent.
    Wall,
}

/// One evaluated candidate in the order it was evaluated.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceEntry<C> {
    /// Zero-based evaluation index.
    pub index: u64,
    /// The index of the candidate this one was proposed from.
    pub parent: Option<u64>,
    /// The candidate.
    pub candidate: C,
    /// What the environment returned.
    pub evaluation: SearchEvaluation,
    /// Whether the strategy moved to this candidate (always true for
    /// best-of-N, which keeps no current state).
    pub accepted: bool,
    /// The annealing temperature of the step; `None` for best-of-N.
    pub temperature: Option<f64>,
    /// Tokens the proposal of this candidate spent.
    pub proposal_tokens: u64,
    /// Trajectories the proposal produced.
    pub proposal_trajectories: Vec<TrajectoryRef>,
}

/// What a search measured about itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchMetrics {
    /// Candidates evaluated.
    pub evaluations: u64,
    /// Candidates the verifier passed.
    pub verified_evaluations: u64,
    /// Whether any candidate was verified.
    pub solved: bool,
    /// How many evaluations it took to reach the first verified one.
    pub evaluations_to_first_verified: Option<u64>,
    /// `verified_evaluations / evaluations`.
    pub solved_per_evaluation: f64,
    /// Tokens spent on proposals and evaluations together.
    pub tokens: u64,
    /// Wall-clock time of the search.
    pub wall_ms: u64,
    /// Distinct candidates evaluated.
    pub distinct_candidates: u64,
    /// `distinct_candidates / evaluations`: 1.0 means no repeats.
    pub diversity: f64,
}

/// The result of a search.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchReport<C> {
    /// Every evaluation, in order, with the trajectories it references.
    pub trace: Vec<TraceEntry<C>>,
    /// The trace index of the verified candidate with the highest utility;
    /// `None` when the verifier passed nothing.
    pub best_verified: Option<u64>,
    /// The trace index of the highest-utility candidate whether verified or
    /// not. Partial credit lives here, never in `best_verified`.
    pub best_utility: Option<u64>,
    /// Why the search ended.
    pub stop: StopReason,
    /// The search's own measurements.
    pub metrics: SearchMetrics,
}

impl<C> SearchReport<C> {
    /// The best verified candidate's trace entry.
    #[must_use]
    pub fn best_verified(&self) -> Option<&TraceEntry<C>> {
        self.best_verified.map(|i| &self.trace[i as usize])
    }

    /// The highest-utility entry, verified or not.
    #[must_use]
    pub fn best_utility(&self) -> Option<&TraceEntry<C>> {
        self.best_utility.map(|i| &self.trace[i as usize])
    }
}
