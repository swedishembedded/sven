// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a paired comparison runs the same tasks under the same seeds in a
//! baseline and a candidate harness, reports per-task outcomes, an
//! uncertainty interval on the mean pass-rate change and the cost of each
//! side, and rejects a candidate that regresses a task.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in comparing
//! agent harness revisions then you can procure our services by sending an
//! email to info@swedishembedded.com.

mod common;

use common::*;
use sven_harness_testkit::{
    compare_paired, run_suite, CaseStatus, Change, HarnessDir, PairedOptions, RunnerOptions,
    Verdict,
};

/// A task whose model answers well under seeds 1 and 2 and misjudges the
/// ticket in its first answer under seeds 3 and 4, so a harness that has no
/// second look at the label loses those seeds.
fn seeded_case(ticket: &str) -> String {
    let good = "{sequence: [{text: '{\"label\": \"bug\"}', usage: {input: 20, output: 5}}, {text: 'bug', usage: {input: 20, output: 5}}]}";
    let spoiled = "{sequence: [{text: '{\"label\": \"feature\"}', usage: {input: 20, output: 5}}, {text: 'bug', usage: {input: 20, output: 5}}]}";
    format!(
        r#"
input: {{ ticket: "{ticket}" }}
model:
  by_seed:
    1: {good}
    2: {good}
    3: {spoiled}
    4: {spoiled}
expect:
  status: completed
  output: {{ queue: bug-queue, label: bug }}
"#
    )
}

fn cases() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let files: Vec<(String, String)> = (1..=4)
        .map(|n| (format!("task{n}.yaml"), seeded_case(&format!("crash {n}"))))
        .collect();
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    write_tree(tmp.path(), &refs);
    tmp
}

fn variant(doc: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_tree(tmp.path(), &[("harness.yaml", doc)]);
    tmp
}

fn options(bootstrap_seed: u64) -> PairedOptions {
    PairedOptions {
        seeds: vec![1, 2, 3, 4],
        bootstrap_seed,
        resamples: 500,
    }
}

#[tokio::test]
async fn a_candidate_that_wins_seeds_the_baseline_loses_is_promoted_with_its_cost() {
    let (a, b, cases) = (variant(ROUTING), variant(&routing_with_review()), cases());
    let report = compare_paired(
        a.path(),
        b.path(),
        cases.path(),
        &RunnerOptions::default(),
        &options(7),
    )
    .await
    .expect("two valid variants compare");

    assert_eq!(report.seeds, vec![1, 2, 3, 4]);
    assert_eq!(report.tasks.len(), 4);
    for task in &report.tasks {
        assert_eq!(task.passed_a, vec![true, true, false, false]);
        assert_eq!(task.passed_b, vec![true, true, true, true]);
        assert!((task.delta - 0.5).abs() < 1e-9);
        assert_eq!(task.change, Change::Fixed);
    }
    assert!((report.mean_delta.value - 0.5).abs() < 1e-9);
    assert!(report.mean_delta.lo > 0.0, "{:?}", report.mean_delta);
    assert!(report.regressions.is_empty());
    assert_eq!(report.decision.verdict, Verdict::Promote);

    // The candidate looks twice, so it spends more model output on every
    // seed; the cost is accounted per run, not hidden.
    let (cost_a, cost_b) = (report.cost.a.output_tokens, report.cost.b.output_tokens);
    assert!(cost_b > cost_a, "{cost_a} vs {cost_b}");
    assert_eq!(report.cost.a.runs, 16);
    assert_eq!(report.cost.b.runs, 16);
}

#[tokio::test]
async fn a_candidate_that_regresses_a_task_is_rejected_by_name() {
    let (a, b, cases) = (
        variant(ROUTING),
        variant(&routing_with_queue_suffix("triage")),
        cases(),
    );
    let report = compare_paired(
        a.path(),
        b.path(),
        cases.path(),
        &RunnerOptions::default(),
        &options(7),
    )
    .await
    .unwrap();
    assert_eq!(report.decision.verdict, Verdict::Reject);
    assert_eq!(report.regressions.len(), 4, "{:?}", report.regressions);
    assert!(report.regressions.contains(&"task1".to_string()));
    assert!(report.mean_delta.value < 0.0);
}

#[tokio::test]
async fn an_identical_candidate_is_inconclusive_not_promoted() {
    let (a, b, cases) = (variant(ROUTING), variant(ROUTING), cases());
    let report = compare_paired(
        a.path(),
        b.path(),
        cases.path(),
        &RunnerOptions::default(),
        &options(7),
    )
    .await
    .unwrap();
    assert_eq!(report.decision.verdict, Verdict::Inconclusive);
    assert!(report.regressions.is_empty());
}

#[tokio::test]
async fn the_interval_is_a_function_of_the_bootstrap_seed_alone() {
    let (a, b, cases) = (variant(ROUTING), variant(&routing_with_review()), cases());
    let run = |seed| {
        let (a, b, cases) = (a.path(), b.path(), cases.path());
        async move {
            compare_paired(a, b, cases, &RunnerOptions::default(), &options(seed))
                .await
                .unwrap()
                .mean_delta
        }
    };
    assert_eq!(run(3).await, run(3).await);
}

#[tokio::test]
async fn a_seeded_model_without_a_seed_is_a_case_error_naming_the_seed() {
    let cases = cases();
    let tmp = variant(ROUTING);
    let dir = HarnessDir::open(tmp.path())
        .unwrap()
        .with_cases_from(cases.path())
        .unwrap();
    let report = run_suite(&dir, &RunnerOptions::default()).await;
    let CaseStatus::Error { message } = &report.cases[0].status else {
        panic!("expected an error, got {:?}", report.cases[0].status);
    };
    assert!(message.contains("seed"), "{message}");
}

#[tokio::test]
async fn pairing_needs_at_least_one_seed() {
    let (a, b, cases) = (variant(ROUTING), variant(ROUTING), cases());
    let none = PairedOptions {
        seeds: Vec::new(),
        bootstrap_seed: 1,
        resamples: 10,
    };
    assert!(compare_paired(
        a.path(),
        b.path(),
        cases.path(),
        &RunnerOptions::default(),
        &none
    )
    .await
    .is_err());
}
