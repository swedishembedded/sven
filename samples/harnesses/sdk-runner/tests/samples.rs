// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The SDK-only runner reproduces the verdicts of the `ticket-routing`
//! harness sample, skips the one case that names a live model, and says
//! plainly what it will not run.
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
async fn the_ticket_routing_sample_passes_through_the_sdk_and_skips_its_live_case() {
    let results = run_dir(&sample("ticket-routing")).await.expect("runs");
    assert_eq!(
        results.len(),
        4,
        "every case of the sample is accounted for"
    );
    for result in &results {
        match result.case.as_str() {
            "live-local" => assert!(
                matches!(&result.outcome, Outcome::Skipped(reason) if reason.contains("sven harness test")),
                "the live case is skipped, never run: {result:?}"
            ),
            _ => assert_eq!(result.outcome, Outcome::Passed, "{result:?}"),
        }
    }
}

#[tokio::test]
async fn a_wrong_expectation_is_reported_as_a_failure() {
    let dir = tempdir_with(
        ONE_TURN,
        "model: { sequence: [ { text: \"fine\" } ] }\nexpect: { status: completed, output: { said: \"other\" } }\n",
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
        let dir = tempdir_with(ONE_TURN, case);
        let error = run_case(dir.path(), &dir.path().join("cases/c.yaml"))
            .await
            .expect_err("refused");
        let message = format!("{error:#}");
        assert!(message.contains(feature), "{message}");
        assert!(message.contains("sven harness test"), "{message}");
    }
}

/// The smallest workflow: one model turn.
const ONE_TURN: &str = "api_version: \"2\"\nname: one\nexecution: { limits: { model_turns: 1 } }\nworkflow:\n  - name: only\n    model_only: true\n    task: Say it.\noutputs:\n  said: { type: string, value: \"{{ tasks.only.output }}\" }\n";

/// A scratch harness directory: `harness.yaml` plus `cases/c.yaml`.
fn tempdir_with(harness: &str, case: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("cases")).unwrap();
    std::fs::write(dir.path().join("harness.yaml"), harness).unwrap();
    std::fs::write(dir.path().join("cases/c.yaml"), case).unwrap();
    dir
}
