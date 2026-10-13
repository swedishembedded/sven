// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: two harness variants run over one case set are compared case by
//! case: which regressed, which got fixed, where the outputs part, what each
//! cost. An unusable variant is a finding, not a crash.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in comparing
//! agent harness revisions then you can procure our services by sending an
//! email to info@swedishembedded.com.

mod common;

use common::*;
use sven_harness_testkit::{compare, Change, RunnerOptions};

fn cases() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_tree(
        tmp.path(),
        &[
            ("bug.yaml", ROUTING_BUG_CASE),
            ("feature.yaml", ROUTING_FEATURE_CASE),
        ],
    );
    tmp
}

fn variant(doc: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    write_tree(tmp.path(), &[("harness.yaml", doc)]);
    tmp
}

#[tokio::test]
async fn a_changed_output_is_a_regression_with_a_diff() {
    let (a, b, cases) = (
        variant(ROUTING),
        variant(&routing_with_bug_queue("triage")),
        cases(),
    );
    let report = compare(a.path(), b.path(), cases.path(), &RunnerOptions::default())
        .await
        .expect("two valid variants compare");

    assert!(report.a.valid && report.b.valid);
    assert_eq!((report.a.passed, report.a.failed), (2, 0));
    assert_eq!((report.b.passed, report.b.failed), (1, 1));
    assert_ne!(report.a.plan_digest, report.b.plan_digest);

    let bug = report.cases.iter().find(|c| c.case == "bug").unwrap();
    assert_eq!(bug.change, Change::Regressed);
    assert!(
        bug.status_changed,
        "the verdict moved from passed to failed"
    );
    assert_eq!(bug.output_diff.len(), 1);
    assert_eq!(bug.output_diff[0].pointer, "/queue");
    assert_eq!(bug.output_diff[0].expected, Some(serde_json::json!("bugs")));
    assert_eq!(bug.output_diff[0].actual, Some(serde_json::json!("triage")));

    let feature = report.cases.iter().find(|c| c.case == "feature").unwrap();
    assert_eq!(feature.change, Change::None);
    assert!(feature.output_diff.is_empty());
}

#[tokio::test]
async fn the_other_direction_is_a_fix() {
    let (a, b, cases) = (
        variant(&routing_with_bug_queue("triage")),
        variant(ROUTING),
        cases(),
    );
    let report = compare(a.path(), b.path(), cases.path(), &RunnerOptions::default())
        .await
        .unwrap();
    let bug = report.cases.iter().find(|c| c.case == "bug").unwrap();
    assert_eq!(bug.change, Change::Fixed);
}

#[tokio::test]
async fn cost_and_steps_are_summed_per_variant() {
    let (a, b, cases) = (variant(ROUTING), variant(ROUTING), cases());
    let report = compare(a.path(), b.path(), cases.path(), &RunnerOptions::default())
        .await
        .unwrap();
    assert_eq!(report.a.output_tokens, report.b.output_tokens);
    assert_eq!(
        report.a.output_tokens,
        Some(6),
        "only the bug case reports usage"
    );
    assert!(report.a.steps > 0);
    assert_eq!(report.a.plan_digest, report.b.plan_digest);
}

#[tokio::test]
async fn an_invalid_variant_is_reported_and_its_cases_count_as_regressions() {
    let a = variant(ROUTING);
    let b = variant("api_version: \"1\"\nname: broken\nentry: nowhere\nsteps: {}\n");
    let cases = cases();
    let report = compare(a.path(), b.path(), cases.path(), &RunnerOptions::default())
        .await
        .expect("an invalid variant does not abort the comparison");

    assert!(report.a.valid);
    assert!(!report.b.valid);
    assert!(report.b.error.is_some());
    assert_eq!(report.cases.len(), 2);
    assert!(report
        .cases
        .iter()
        .all(|c| c.change == Change::Regressed && c.b.is_none()));
}

#[tokio::test]
async fn a_missing_variant_path_is_an_error() {
    let cases = cases();
    let a = variant(ROUTING);
    let missing = a.path().join("nowhere");
    assert!(
        compare(a.path(), &missing, cases.path(), &RunnerOptions::default())
            .await
            .is_err()
    );
}
