// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: nothing in the testkit reaches a real model unless the caller opts in
//! explicitly with an endpoint (`--model-endpoint`) on the command line.
//! Without it a case that names a live model is skipped, a case without a
//! model gets a model that only fails, and no shipped sample may ask for a
//! live model except the one case that exists for that purpose.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in keeping
//! autonomous-workflow tests hermetic then you can procure our services by
//! sending an email to info@swedishembedded.com.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::json;
use sven_harness_testkit::{
    run_case, run_once, run_suite, CaseStatus, OptIn, RunRequest, RunStatus, RunnerOptions,
};

const LIVE_CASE: &str = r#"
input: { ticket: "x" }
model: { live: { model: "ollama/qwen3:8b" } }
expect: { status: completed }
"#;

fn skipped_reason(status: &CaseStatus) -> &str {
    match status {
        CaseStatus::Skipped { reason } => reason,
        other => panic!("expected a skipped case, got {other:?}"),
    }
}

#[tokio::test]
async fn a_live_case_is_skipped_without_the_opt_in() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", ROUTING), ("cases/live.yaml", LIVE_CASE)]);
    let report = run_case(&dir, &dir.cases[0], &RunnerOptions::default()).await;
    assert!(skipped_reason(&report.status).contains("--model-endpoint"));
    assert!(report.run.is_none(), "a skipped case never runs");

    let suite = run_suite(&dir, &RunnerOptions::default()).await;
    assert_eq!((suite.passed, suite.failed, suite.skipped), (0, 0, 1));
}

#[tokio::test]
async fn a_live_model_needs_an_explicit_endpoint() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", ROUTING), ("cases/live.yaml", LIVE_CASE)]);
    for blank in ["", "   "] {
        let options = RunnerOptions {
            opt_in: OptIn::live(Some("ollama/qwen3:8b"), blank),
            ..RunnerOptions::default()
        };
        let report = run_case(&dir, &dir.cases[0], &options).await;
        assert!(
            skipped_reason(&report.status).contains("--model-endpoint"),
            "{:?}",
            report.status
        );
    }
}

#[tokio::test]
async fn run_once_refuses_a_live_model_without_the_opt_in() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", ROUTING)]);
    let request = RunRequest {
        input: json!({"ticket": "x"}),
        model: Some(serde_yaml::from_str("live: { model: \"ollama/qwen3:8b\" }").unwrap()),
        ..RunRequest::default()
    };
    let error = run_once(&dir, request)
        .await
        .expect_err("no opt-in, no live model");
    assert!(error.to_string().contains("--model-endpoint"), "{error}");
}

#[test]
fn the_opt_in_is_constructed_only_from_an_explicit_endpoint() {
    assert!(!OptIn::none().is_live_ready());
    assert!(!OptIn::default().is_live_ready());
    assert!(OptIn::live(Some("ollama/qwen3:8b"), "http://localhost:11434").is_live_ready());
    assert!(OptIn::live(None, "http://localhost:11434").is_live_ready());
    assert!(
        !OptIn::live(Some("ollama/qwen3:8b"), "  ").is_live_ready(),
        "a blank endpoint is none"
    );
}

#[tokio::test]
async fn a_case_without_a_model_never_reaches_configuration() {
    // A model-free harness (a check task over a scripted gate) completes
    // with no model of any kind.
    let case = r#"
input: {}
tools:
  shell_gate:
    capability: ExecuteShell
    responses:
      - ok: { exit_code: 0, timed_out: false, error: null, stdout: "all clear", stderr: "" }
expect: { status: completed, output: { report: "all clear" } }
"#;
    let (_tmp, dir) = harness_dir(&[("harness.yaml", REPORT), ("cases/c.yaml", case)]);
    let report = run_case(&dir, &dir.cases[0], &RunnerOptions::default()).await;
    assert_eq!(report.status, CaseStatus::Passed, "{:?}", report.mismatches);

    // A model step with no fixture meets a provider that only fails, never a
    // configured one.
    let (_tmp, dir) = harness_dir(&[("harness.yaml", TWO_TURNS)]);
    let result = run_once(
        &dir,
        RunRequest {
            input: json!({"ticket": "x"}),
            ..RunRequest::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(result.summary.status, RunStatus::Failed);
    assert!(result
        .summary
        .failure
        .unwrap()
        .message
        .contains("no model fixture"));

    // And the crate's sources never ask for the user's configuration.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for forbidden in ["config::load", "Config::load"] {
            assert!(
                !text.contains(forbidden),
                "{} reads the user's configuration",
                path.display()
            );
        }
        assert!(
            !text.contains("env::var"),
            "{} reads the environment: the live opt-in is command-line flags only",
            path.display()
        );
    }
}

/// The only case file name that may name a live model.
const LIVE_CASE_FILE: &str = "live-local.yaml";

/// Case files under `samples_root/*/cases/` that name a live model, except
/// those named [`LIVE_CASE_FILE`].
fn live_cases_outside_the_allowed_file(samples_root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(samples) = std::fs::read_dir(samples_root) else {
        return found; // no samples directory yet: nothing can name a live model
    };
    for sample in samples.flatten() {
        let Ok(cases) = std::fs::read_dir(sample.path().join("cases")) else {
            continue;
        };
        for case in cases.flatten() {
            if case.file_name() == LIVE_CASE_FILE {
                continue;
            }
            let text = std::fs::read_to_string(case.path()).unwrap_or_default();
            let doc: serde_yaml::Value =
                serde_yaml::from_str(&text).unwrap_or(serde_yaml::Value::Null);
            if doc.get("model").and_then(|m| m.get("live")).is_some() {
                found.push(case.path());
            }
        }
    }
    found
}

#[test]
fn the_sample_scan_finds_a_live_case_in_any_file_but_live_local() {
    let tmp = tempfile::tempdir().unwrap();
    write_tree(
        tmp.path(),
        &[
            ("ticket-routing/cases/ok.yaml", ROUTING_BUG_CASE),
            ("ticket-routing/cases/live-local.yaml", LIVE_CASE),
            ("sneaky/cases/live.yaml", LIVE_CASE),
        ],
    );
    let found = live_cases_outside_the_allowed_file(tmp.path());
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("sneaky/cases/live.yaml"));
    assert!(live_cases_outside_the_allowed_file(&tmp.path().join("absent")).is_empty());
}

#[test]
fn no_default_sample_names_a_live_model() {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/harnesses");
    let offenders = live_cases_outside_the_allowed_file(&samples);
    assert!(
        offenders.is_empty(),
        "only a case file named {LIVE_CASE_FILE} may use `live`: {offenders:?}"
    );
}
