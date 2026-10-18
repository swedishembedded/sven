// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The strategies and the loop that drives them.
//!
//! Swedish Embedded AB implements reproducible agent search for its clients.
//! If your team needs expertise in verifier-guided search over agent
//! candidates then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::rng::SearchRng;
use super::types::{
    EvalLimits, ProposalStep, SearchBudget, SearchMetrics, SearchReport, StopReason, TraceEntry,
};
use super::{CandidateEnvironment, Proposer};

/// How the search moves between candidates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Strategy {
    /// Evaluate the initial candidate, then propose every other candidate
    /// independently from it. The budget's `max_evaluations` is N.
    BestOfN,
    /// Metropolis walk: propose from the current candidate, always accept an
    /// improvement, accept a worse candidate with probability
    /// `exp(delta / T)`, cool `T` geometrically each step.
    SimulatedAnnealing {
        /// The starting temperature, in utility units; positive and finite.
        initial_temperature: f64,
        /// The factor `T` is multiplied by each step, in `(0, 1]`.
        cooling: f64,
    },
}

/// Why a search could not run or stopped.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The budget or strategy parameters are unusable.
    #[error("invalid search configuration: {0}")]
    Invalid(String),
    /// The environment returned a utility that is not a finite number.
    #[error("evaluation {index} returned a non-finite utility ({utility})")]
    NonFiniteUtility {
        /// The evaluation's index.
        index: u64,
        /// What it returned.
        utility: f64,
    },
    /// The proposer failed.
    #[error("proposal for evaluation {index} failed: {source}")]
    Proposer {
        /// The evaluation the proposal was for.
        index: u64,
        /// The cause.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The evaluation infrastructure failed.
    #[error("evaluation {index} failed: {source}")]
    Environment {
        /// The evaluation's index.
        index: u64,
        /// The cause.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// A candidate could not be serialized to its identity key.
    #[error("candidate cannot be serialized: {0}")]
    Candidate(#[from] serde_json::Error),
}

impl SearchError {
    /// A proposer failure at `index` (for [`Proposer`] implementations that
    /// have no index at hand, `index` is filled in by the search).
    pub fn proposer(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Proposer {
            index: 0,
            source: source.into(),
        }
    }

    /// An environment failure.
    pub fn environment(source: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Self {
        Self::Environment {
            index: 0,
            source: source.into(),
        }
    }

    fn at(self, at: u64) -> Self {
        match self {
            Self::Proposer { source, .. } => Self::Proposer { index: at, source },
            Self::Environment { source, .. } => Self::Environment { index: at, source },
            other => other,
        }
    }
}

fn validate(strategy: &Strategy, budget: &SearchBudget) -> Result<(), SearchError> {
    let invalid = |m: &str| Err(SearchError::Invalid(m.to_string()));
    if budget.max_evaluations == 0 {
        return invalid("max_evaluations must be at least 1");
    }
    if let Strategy::SimulatedAnnealing {
        initial_temperature,
        cooling,
    } = strategy
    {
        if !(initial_temperature.is_finite() && *initial_temperature > 0.0) {
            return invalid("initial_temperature must be positive and finite");
        }
        if !(cooling.is_finite() && *cooling > 0.0 && *cooling <= 1.0) {
            return invalid("cooling must be in (0, 1]");
        }
    }
    Ok(())
}

/// Searches from `initial` under `budget`, reproducibly from `seed`.
///
/// # Errors
///
/// [`SearchError`] for unusable parameters, a candidate that cannot be
/// serialized, a non-finite utility, or a proposer or environment failure.
/// A candidate that merely fails verification is not an error.
pub async fn search<C, P, E>(
    strategy: Strategy,
    initial: C,
    proposer: &P,
    environment: &E,
    budget: &SearchBudget,
    seed: u64,
) -> Result<SearchReport<C>, SearchError>
where
    C: Clone + Send + Sync + Serialize,
    P: Proposer<C>,
    E: CandidateEnvironment<C>,
{
    validate(&strategy, budget)?;
    let started = Instant::now();
    let mut rng = SearchRng::from_seed(seed);
    let mut trace: Vec<TraceEntry<C>> = Vec::new();
    let mut tokens = 0u64;
    let mut current: Option<usize> = None;
    let mut temperature = match strategy {
        Strategy::BestOfN => None,
        Strategy::SimulatedAnnealing {
            initial_temperature,
            ..
        } => Some(initial_temperature),
    };
    let stop = loop {
        let index = trace.len() as u64;
        if let Some(reason) = exhausted(budget, index, tokens, started.elapsed()) {
            break reason;
        }
        let (candidate, parent, proposal_tokens, proposal_trajectories) = match (index, current) {
            (0, _) | (_, None) => (initial.clone(), None, 0, Vec::new()),
            (_, Some(at)) => {
                let step = ProposalStep { index, temperature };
                let parent = &trace[at];
                let proposal = proposer
                    .propose(&parent.candidate, &step, &mut rng)
                    .await
                    .map_err(|e| e.at(index))?;
                (
                    proposal.candidate,
                    Some(parent.index),
                    proposal.tokens,
                    proposal.trajectories,
                )
            }
        };
        tokens += proposal_tokens;
        let limits = EvalLimits {
            tokens_remaining: budget.max_tokens.map(|m| m.saturating_sub(tokens)),
            wall_remaining: budget.max_wall.map(|m| m.saturating_sub(started.elapsed())),
        };
        let evaluation = environment
            .evaluate(&candidate, &limits)
            .await
            .map_err(|e| e.at(index))?;
        if !evaluation.utility.is_finite() {
            return Err(SearchError::NonFiniteUtility {
                index,
                utility: evaluation.utility,
            });
        }
        tokens += evaluation.tokens;
        let accepted = match (current, temperature) {
            (None, _) | (_, None) => true,
            (Some(at), Some(t)) => {
                let delta = evaluation.utility - trace[at].evaluation.utility;
                delta >= 0.0 || rng.unit() < (delta / t).exp()
            }
        };
        let verified = evaluation.verified();
        trace.push(TraceEntry {
            index,
            parent,
            candidate,
            evaluation,
            accepted,
            temperature,
            proposal_tokens,
            proposal_trajectories,
        });
        // Best-of-N proposes every candidate from the initial one, so its
        // "current" never leaves entry 0; annealing moves on acceptance.
        match strategy {
            Strategy::BestOfN => current = Some(0),
            Strategy::SimulatedAnnealing { cooling, .. } => {
                if accepted || current.is_none() {
                    current = Some(trace.len() - 1);
                }
                temperature = temperature.map(|t| t * cooling);
            }
        }
        if verified && budget.stop_on_verified {
            break StopReason::Verified;
        }
    };
    report(trace, stop, tokens, started.elapsed())
}

fn exhausted(
    budget: &SearchBudget,
    evaluated: u64,
    tokens: u64,
    wall: Duration,
) -> Option<StopReason> {
    if evaluated >= budget.max_evaluations {
        Some(StopReason::Evaluations)
    } else if budget.max_tokens.is_some_and(|m| tokens >= m) {
        Some(StopReason::Tokens)
    } else if budget.max_wall.is_some_and(|m| wall >= m) {
        Some(StopReason::Wall)
    } else {
        None
    }
}

fn report<C: Serialize>(
    trace: Vec<TraceEntry<C>>,
    stop: StopReason,
    tokens: u64,
    wall: Duration,
) -> Result<SearchReport<C>, SearchError> {
    let mut keys = BTreeSet::new();
    for entry in &trace {
        keys.insert(serde_json::to_string(&entry.candidate)?);
    }
    let best = |only_verified: bool| {
        trace
            .iter()
            .filter(|e| !only_verified || e.evaluation.verified())
            // Earliest wins ties, so the report does not depend on float
            // equality quirks of later candidates.
            .fold(None::<&TraceEntry<C>>, |best, e| match best {
                Some(b) if b.evaluation.utility >= e.evaluation.utility => Some(b),
                _ => Some(e),
            })
            .map(|e| e.index)
    };
    let evaluations = trace.len() as u64;
    let verified_evaluations = trace.iter().filter(|e| e.evaluation.verified()).count() as u64;
    let metrics = SearchMetrics {
        evaluations,
        verified_evaluations,
        solved: verified_evaluations > 0,
        evaluations_to_first_verified: trace
            .iter()
            .position(|e| e.evaluation.verified())
            .map(|p| p as u64 + 1),
        solved_per_evaluation: verified_evaluations as f64 / evaluations.max(1) as f64,
        tokens,
        wall_ms: u64::try_from(wall.as_millis()).unwrap_or(u64::MAX),
        distinct_candidates: keys.len() as u64,
        diversity: keys.len() as f64 / evaluations.max(1) as f64,
    };
    Ok(SearchReport {
        best_verified: best(true),
        best_utility: best(false),
        trace,
        stop,
        metrics,
    })
}
