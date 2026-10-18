// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the search facility on a deterministic environment with a known
//! solution and partial credit. Utility guides the search; only the
//! verifier's verdict makes a candidate correct; the same seed reproduces a
//! search; budgets bound it.
//!
//! The environment is a hidden bit string. A candidate is a bit string, its
//! utility the fraction of positions that match (partial credit) and the
//! verifier passes it only when every position matches.
//!
//! Swedish Embedded AB implements reproducible agent search for its clients.
//! If your team needs expertise in verifier-guided search over agent
//! candidates then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::BTreeMap;
use std::time::Duration;

use sven_sdk::search::{
    search, CandidateEnvironment, EvalLimits, Proposal, ProposalStep, Proposer, SearchBudget,
    SearchError, SearchEvaluation, SearchRng, StopReason, Strategy, TrajectoryRef, VerifierVerdict,
};

const BITS: usize = 24;
const TOKENS_PER_EVALUATION: u64 = 10;

type Bits = Vec<bool>;

/// The hidden solution: every third bit set, plus the last.
fn target() -> Bits {
    (0..BITS).map(|i| i % 3 == 0 || i == BITS - 1).collect()
}

struct BitEnvironment;

impl CandidateEnvironment<Bits> for BitEnvironment {
    async fn evaluate(
        &self,
        candidate: &Bits,
        _limits: &EvalLimits,
    ) -> Result<SearchEvaluation, SearchError> {
        let matched = candidate
            .iter()
            .zip(target())
            .filter(|(a, b)| **a == *b)
            .count();
        let solved = matched == BITS;
        Ok(SearchEvaluation {
            terminal: Some(if solved {
                VerifierVerdict::Passed
            } else {
                VerifierVerdict::Failed {
                    reason: format!("{} of {BITS} bits differ", BITS - matched),
                }
            }),
            utility: matched as f64 / BITS as f64,
            signals: BTreeMap::from([("matched".to_string(), matched as f64)]),
            tokens: TOKENS_PER_EVALUATION,
            trajectories: vec![TrajectoryRef {
                run_id: format!("eval-{matched}"),
                location: None,
            }],
        })
    }
}

/// Flips one random bit of the parent.
struct FlipOne;

impl Proposer<Bits> for FlipOne {
    async fn propose(
        &self,
        parent: &Bits,
        _step: &ProposalStep,
        rng: &mut SearchRng,
    ) -> Result<Proposal<Bits>, SearchError> {
        let mut child = parent.clone();
        let at = rng.below(child.len());
        child[at] = !child[at];
        Ok(Proposal::free(child))
    }
}

fn budget(evaluations: u64) -> SearchBudget {
    SearchBudget {
        max_evaluations: evaluations,
        max_tokens: None,
        max_wall: None,
        stop_on_verified: true,
    }
}

const ANNEALING: Strategy = Strategy::SimulatedAnnealing {
    initial_temperature: 0.05,
    cooling: 0.995,
};

#[tokio::test]
async fn annealing_finds_the_known_solution_and_reports_how_fast() {
    let report = search(
        ANNEALING,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &budget(2000),
        11,
    )
    .await
    .expect("the search runs");

    assert_eq!(report.stop, StopReason::Verified);
    let best = report
        .best_verified()
        .expect("the verifier passed a candidate");
    assert_eq!(best.candidate, target());
    assert_eq!(best.evaluation.terminal, Some(VerifierVerdict::Passed));

    let m = &report.metrics;
    assert!(m.solved);
    assert_eq!(m.verified_evaluations, 1, "stopped on the first verified");
    assert_eq!(m.evaluations_to_first_verified, Some(m.evaluations));
    assert!(m.evaluations < 2000, "{m:?}");
    assert_eq!(m.tokens, m.evaluations * TOKENS_PER_EVALUATION);
    assert!(m.diversity > 0.0 && m.diversity <= 1.0);
    assert!((m.solved_per_evaluation - 1.0 / m.evaluations as f64).abs() < 1e-12);
    // The trace references the trajectory of every evaluation.
    assert_eq!(report.trace.len() as u64, m.evaluations);
    assert!(report
        .trace
        .iter()
        .all(|e| !e.evaluation.trajectories.is_empty()));
}

#[tokio::test]
async fn partial_credit_is_never_reported_as_a_verified_candidate() {
    // Best-of-N proposes single flips of the initial candidate, which cannot
    // reach the solution: utility rises above the start, the verifier never
    // passes, and the report says so.
    let report = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &budget(300),
        11,
    )
    .await
    .unwrap();

    assert_eq!(report.stop, StopReason::Evaluations);
    assert!(report.best_verified().is_none());
    assert!(!report.metrics.solved);
    assert_eq!(report.metrics.evaluations_to_first_verified, None);
    let partial = report.best_utility().expect("some candidate scored");
    assert!(partial.evaluation.utility > report.trace[0].evaluation.utility);
    assert!(!partial.evaluation.verified());
}

#[tokio::test]
async fn the_same_seed_reproduces_the_search_and_another_seed_does_not() {
    let run = |seed| async move {
        search(
            ANNEALING,
            vec![false; BITS],
            &FlipOne,
            &BitEnvironment,
            &budget(60),
            seed,
        )
        .await
        .unwrap()
        .trace
    };
    assert_eq!(run(5).await, run(5).await);
    assert_ne!(run(5).await, run(6).await);
}

#[tokio::test]
async fn the_token_budget_bounds_the_search() {
    let bounded = SearchBudget {
        max_tokens: Some(55),
        ..budget(1000)
    };
    let report = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &bounded,
        1,
    )
    .await
    .unwrap();
    assert_eq!(report.stop, StopReason::Tokens);
    assert_eq!(report.metrics.evaluations, 6, "55 tokens allow 6 x 10");
}

#[tokio::test]
async fn the_wall_budget_bounds_the_search() {
    let bounded = SearchBudget {
        max_wall: Some(Duration::ZERO),
        ..budget(1000)
    };
    let report = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &bounded,
        1,
    )
    .await
    .unwrap();
    assert_eq!(report.stop, StopReason::Wall);
    assert_eq!(report.metrics.evaluations, 0);
    assert!(report.best_utility().is_none());
}

#[tokio::test]
async fn without_stop_on_verified_the_budget_is_spent_and_the_best_verified_kept() {
    let keep_going = SearchBudget {
        stop_on_verified: false,
        ..budget(400)
    };
    let report = search(
        ANNEALING,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &keep_going,
        11,
    )
    .await
    .unwrap();
    assert_eq!(report.stop, StopReason::Evaluations);
    assert_eq!(report.metrics.evaluations, 400);
    assert!(report.metrics.solved);
    assert!(report.best_verified().is_some());
}

#[tokio::test]
async fn unusable_parameters_are_refused_before_anything_runs() {
    let none = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &budget(0),
        1,
    )
    .await;
    assert!(matches!(none, Err(SearchError::Invalid(_))));
    let hot = search(
        Strategy::SimulatedAnnealing {
            initial_temperature: 0.0,
            cooling: 0.9,
        },
        vec![false; BITS],
        &FlipOne,
        &BitEnvironment,
        &budget(10),
        1,
    )
    .await;
    assert!(matches!(hot, Err(SearchError::Invalid(_))));
}

#[tokio::test]
async fn an_environment_failure_aborts_with_the_evaluation_index() {
    struct Broken;
    impl CandidateEnvironment<Bits> for Broken {
        async fn evaluate(
            &self,
            _: &Bits,
            _: &EvalLimits,
        ) -> Result<SearchEvaluation, SearchError> {
            Err(SearchError::environment("sandbox unavailable"))
        }
    }
    let err = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &Broken,
        &budget(5),
        1,
    )
    .await
    .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("evaluation 0") && text.contains("sandbox unavailable"),
        "{text}"
    );
}

#[tokio::test]
async fn a_non_finite_utility_is_an_error_not_a_ranking() {
    struct Nan;
    impl CandidateEnvironment<Bits> for Nan {
        async fn evaluate(
            &self,
            _: &Bits,
            _: &EvalLimits,
        ) -> Result<SearchEvaluation, SearchError> {
            Ok(SearchEvaluation {
                terminal: None,
                utility: f64::NAN,
                signals: BTreeMap::new(),
                tokens: 0,
                trajectories: Vec::new(),
            })
        }
    }
    let err = search(
        Strategy::BestOfN,
        vec![false; BITS],
        &FlipOne,
        &Nan,
        &budget(5),
        1,
    )
    .await
    .unwrap_err();
    assert!(matches!(
        err,
        SearchError::NonFiniteUtility { index: 0, .. }
    ));
}

#[tokio::test]
async fn over_many_seeds_annealing_solves_what_best_of_n_cannot() {
    const SEEDS: u64 = 20;
    const EVALUATIONS: u64 = 1000;
    let tally = |strategy: Strategy| {
        let budget = budget(EVALUATIONS);
        async move {
            let (mut solved, mut evaluations, mut diversity) = (0u64, 0u64, 0.0);
            for seed in 0..SEEDS {
                let report = search(
                    strategy,
                    vec![false; BITS],
                    &FlipOne,
                    &BitEnvironment,
                    &budget,
                    seed,
                )
                .await
                .unwrap();
                solved += u64::from(report.metrics.solved);
                evaluations += report.metrics.evaluations;
                diversity += report.metrics.diversity;
            }
            (solved, evaluations, diversity / SEEDS as f64)
        }
    };
    let (sa_solved, sa_evals, sa_div) = tally(ANNEALING).await;
    let (bon_solved, bon_evals, bon_div) = tally(Strategy::BestOfN).await;
    eprintln!(
        "annealing: solved {sa_solved}/{SEEDS}, {sa_evals} evaluations, diversity {sa_div:.3}; \
         best-of-n: solved {bon_solved}/{SEEDS}, {bon_evals} evaluations, diversity {bon_div:.3}"
    );
    assert_eq!(bon_solved, 0);
    assert!(
        sa_solved == SEEDS,
        "annealing solved only {sa_solved}/{SEEDS}"
    );
    assert!(
        sa_evals < bon_evals,
        "solving stops annealing early; best-of-n always spends the budget"
    );
}
