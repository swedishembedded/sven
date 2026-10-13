// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SDK-only runner reproduces the verdicts of the first two harness
//! samples, and says plainly what it will not run.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in running agent workflows from your own
//! application then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::path::{Path, PathBuf};

use sample_harnesses_sdk_runner::{run_case, run_dir, Outcome};

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(name)
}

#[tokio::test]
async fn the_decision_sample_passes_through_the_sdk() {
    let results = run_dir(&sample("01-decision")).await.expect("runs");
    assert_eq!(results.len(), 4, "every case of the sample ran");
    for result in &results {
        assert_eq!(result.outcome, Outcome::Passed, "{result:?}");
    }
}

#[tokio::test]
async fn the_model_routing_sample_passes_through_the_sdk() {
    let results = run_dir(&sample("02-model-routing")).await.expect("runs");
    assert_eq!(results.len(), 4, "every case of the sample ran");
    for result in &results {
        assert_eq!(result.outcome, Outcome::Passed, "{result:?}");
    }
}

#[tokio::test]
async fn a_wrong_expectation_is_reported_as_a_failure() {
    let dir = tempdir_with(
        "api_version: \"1\"\nname: one\nentry: done\nsteps:\n  done: { return: { value: 1 } }\n",
        "expect: { status: completed, output: 2 }\n",
    );
    let result = run_case(dir.path(), &dir.path().join("cases/c.yaml"))
        .await
        .expect("runs");
    match result.outcome {
        Outcome::Failed(reasons) => assert!(reasons[0].contains("output"), "{reasons:?}"),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[tokio::test]
async fn cases_that_need_the_testkit_are_refused_with_a_pointer() {
    for (case, feature) in [
        (
            "tools: { read_notes: { capability: ReadFile } }\nexpect: { status: completed }\n",
            "tools",
        ),
        (
            "model: { rules: [] }\nexpect: { status: completed }\n",
            "rules",
        ),
        (
            "model: { live: { model: \"ollama/qwen3:8b\" } }\nexpect: { status: completed }\n",
            "live",
        ),
    ] {
        let dir = tempdir_with(
            "api_version: \"1\"\nname: one\nentry: done\nsteps:\n  done: { return: { value: 1 } }\n",
            case,
        );
        let error = run_case(dir.path(), &dir.path().join("cases/c.yaml"))
            .await
            .expect_err("refused");
        let message = format!("{error:#}");
        assert!(message.contains(feature), "{message}");
        assert!(message.contains("sven harness test"), "{message}");
    }
}

/// A scratch harness directory: `harness.yaml` plus `cases/c.yaml`.
fn tempdir_with(harness: &str, case: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cases")).unwrap();
    std::fs::write(dir.path().join("harness.yaml"), harness).unwrap();
    std::fs::write(dir.path().join("cases/c.yaml"), case).unwrap();
    dir
}
