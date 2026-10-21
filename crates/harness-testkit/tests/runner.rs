// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the runner executes a harness against fixture models and tools on a
//! fresh engine per case, judges the run against the case's expectations,
//! and proves the run replays offline to the same verdict.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod common;

use common::*;
use serde_json::json;
use sven_harness_testkit::{
    replay_once, resume_once, run_case, run_once, run_suite, CaseStatus, Mismatch, ModelFixture,
    RunRequest, RunStatus, RunnerOptions,
};

fn opts() -> RunnerOptions {
    RunnerOptions::default()
}

async fn only_case(files: &[(&str, &str)]) -> sven_harness_testkit::CaseReport {
    let (_tmp, dir) = harness_dir(files);
    let case = dir.cases[0].clone();
    run_case(&dir, &case, &opts()).await
}

fn names(mismatches: &[Mismatch]) -> Vec<&str> {
    mismatches.iter().map(|m| m.expectation.as_str()).collect()
}

#[tokio::test]
async fn a_completed_case_passes_and_reports_its_run() {
    let report = only_case(&[
        ("harness.yaml", ROUTING),
        ("cases/bug.yaml", ROUTING_BUG_CASE),
    ])
    .await;

    assert_eq!(report.case, "bug");
    assert_eq!(report.status, CaseStatus::Passed, "{:?}", report.mismatches);
    let run = report.run.expect("a case that ran reports its run");
    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(
        run.output,
        Some(json!({"queue": "bug-queue", "label": "bug"}))
    );
    assert_eq!(run.step_counts["classify__work"], 1);
    assert_eq!(run.input_tokens, Some(20));
    assert_eq!(run.output_tokens, Some(6));
    assert!(!run.plan_digest.is_empty());
    assert!(
        report.replay_checked,
        "every case is replay-checked by default"
    );
}

#[tokio::test]
async fn a_failed_run_is_judged_by_its_failure_object() {
    let report = only_case(&[
        ("harness.yaml", ROUTING),
        ("cases/unroutable.yaml", ROUTING_UNROUTABLE_CASE),
    ])
    .await;
    assert_eq!(report.status, CaseStatus::Passed, "{:?}", report.mismatches);
    let run = report.run.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.failure.unwrap().code, "invalid_answer");
}

#[tokio::test]
async fn a_wrong_expectation_fails_the_case_and_names_the_step() {
    let wrong = ROUTING_BUG_CASE.replace(
        "steps: { classify__work: 1 }",
        "steps: { classify__work: 2 }",
    );
    let report = only_case(&[("harness.yaml", ROUTING), ("cases/bug.yaml", &wrong)]).await;

    assert_eq!(report.status, CaseStatus::Failed);
    let mismatch = report
        .mismatches
        .iter()
        .find(|m| m.expectation == "steps.classify__work")
        .unwrap_or_else(|| panic!("the mismatch names the step: {:?}", report.mismatches));
    assert_eq!(mismatch.expected, json!(2));
    assert_eq!(mismatch.actual, json!(1));
}

#[tokio::test]
async fn the_unexpected_status_is_a_mismatch_with_the_failure_in_view() {
    let wrong = ROUTING_UNROUTABLE_CASE.replace("status: failed", "status: completed");
    let report = only_case(&[("harness.yaml", ROUTING), ("cases/c.yaml", &wrong)]).await;
    assert_eq!(report.status, CaseStatus::Failed);
    assert!(
        names(&report.mismatches).contains(&"status"),
        "{:?}",
        report.mismatches
    );
}

#[tokio::test]
async fn running_out_of_tool_responses_is_a_case_error() {
    let case = r#"
input: {}
tools: { shell_gate: { capability: ExecuteShell, responses: [] } }
expect: { status: failed }
"#;
    let report = only_case(&[("harness.yaml", REPORT), ("cases/exhausted.yaml", case)]).await;
    match &report.status {
        CaseStatus::Error { message } => {
            assert!(message.contains("shell_gate"), "names the tool: {message}");
            assert!(message.contains("exhausted"), "says why: {message}");
        }
        other => panic!("fixture exhaustion is an error, not {other:?}"),
    }
}

#[tokio::test]
async fn running_out_of_model_answers_is_a_case_error() {
    let case = r#"
input: { ticket: "x" }
model: { sequence: [ { text: '{"label": "bug"}' } ] }
expect: { status: completed }
"#;
    let report = only_case(&[("harness.yaml", TWO_TURNS), ("cases/c.yaml", case)]).await;
    assert!(
        matches!(&report.status, CaseStatus::Error { message } if message.contains("model")),
        "{:?}",
        report.status
    );
}

#[tokio::test]
async fn cancelling_on_a_tool_the_case_does_not_define_is_a_manifest_error() {
    let case = r#"
input: {}
tools: { shell_gate: { capability: ExecuteShell, responses: [ { ok: "x" } ] } }
bounds: { cancel_on_tool_start: nothing_like_it }
expect: { status: completed }
"#;
    let report = only_case(&[("harness.yaml", REPORT), ("cases/cancel.yaml", case)]).await;
    assert!(
        matches!(&report.status, CaseStatus::Error { message } if message.contains("nothing_like_it")),
        "{:?}",
        report.status
    );
}

#[tokio::test]
async fn a_blocking_tool_is_cancelled_and_leaves_nothing_running() {
    let case = r#"
input: {}
tools: { shell_gate: { capability: ExecuteShell, responses: [ { block: true } ] } }
bounds: { cancel_on_tool_start: shell_gate }
expect: { status: cancelled, tools_invoked: [shell_gate] }
"#;
    let report = only_case(&[("harness.yaml", REPORT), ("cases/cancel.yaml", case)]).await;
    assert_eq!(
        report.status,
        CaseStatus::Passed,
        "{:?} {:?}",
        report.status,
        report.mismatches
    );
    assert_eq!(report.run.unwrap().status, RunStatus::Cancelled);
}

const SUSPEND_THEN_RESUME: &str = r#"
input: {}
tools: { shell_gate: { capability: ExecuteShell, responses: [ { block: true } ] } }
bounds: { deadline_ms: 300 }
expect: { status: suspended }
resume:
  tools:
    shell_gate:
      capability: ExecuteShell
      responses:
        - ok: { exit_code: 0, timed_out: false, error: null, stdout: "the report", stderr: "" }
  expect:
    status: completed
    output: { report: "the report" }
    steps: { fetch__v0_read_a0: 1 }
"#;

#[tokio::test]
async fn a_suspended_run_resumes_on_its_checkpoint_in_the_same_case() {
    let report = only_case(&[
        ("harness.yaml", REPORT),
        ("cases/resume.yaml", SUSPEND_THEN_RESUME),
    ])
    .await;
    assert_eq!(
        report.status,
        CaseStatus::Passed,
        "{:?} {:?}",
        report.status,
        report.mismatches
    );
    assert!(report.replay_checked);
}

#[tokio::test]
async fn a_resume_expectation_that_does_not_hold_is_reported_under_its_phase() {
    let wrong = SUSPEND_THEN_RESUME.replace("fetch__v0_read_a0: 1", "fetch__v0_read_a0: 5");
    let report = only_case(&[("harness.yaml", REPORT), ("cases/resume.yaml", &wrong)]).await;
    assert_eq!(report.status, CaseStatus::Failed);
    assert!(
        names(&report.mismatches).contains(&"resume.steps.fetch__v0_read_a0"),
        "{:?}",
        report.mismatches
    );
}

#[tokio::test]
async fn a_tool_can_write_into_the_case_workspace_for_a_later_verification() {
    let case = format!(
        r#"
input: {{}}
{PATCHING_MODEL}
tools:
  write_patch:
    capability: WriteFile
    responses:
      - ok: "patched"
        write: {{ path: fixed.txt, content: "ok" }}
expect: {{ status: completed, output: {{ note: "done" }}, tools_invoked: [write_patch] }}
"#
    );
    let report = only_case(&[("harness.yaml", REPAIR), ("cases/repair.yaml", &case)]).await;
    assert_eq!(
        report.status,
        CaseStatus::Passed,
        "{:?} {:?}",
        report.status,
        report.mismatches
    );
}

#[tokio::test]
async fn a_workspace_file_is_visible_to_the_run_and_the_fixture_write_is_jailed() {
    let seeded = format!(
        r#"
input: {{}}
{PATCHING_MODEL}
workspace: {{ "fixed.txt": "already here" }}
tools: {{ write_patch: {{ capability: WriteFile, responses: [ {{ ok: "noop" }} ] }} }}
expect: {{ status: completed, output: {{ note: "done" }} }}
"#
    );
    let report = only_case(&[("harness.yaml", REPAIR), ("cases/seeded.yaml", &seeded)]).await;
    assert_eq!(
        report.status,
        CaseStatus::Passed,
        "{:?} {:?}",
        report.status,
        report.mismatches
    );

    let escape = format!(
        r#"
input: {{}}
{PATCHING_MODEL}
tools:
  write_patch:
    capability: WriteFile
    responses: [ {{ ok: "x", write: {{ path: "../escaped.txt", content: "no" }} }} ]
expect: {{ status: failed }}
"#
    );
    let report = only_case(&[("harness.yaml", REPAIR), ("cases/escape.yaml", &escape)]).await;
    assert!(
        matches!(&report.status, CaseStatus::Error { message } if message.contains("outside")),
        "a fixture write cannot leave the workspace: {:?}",
        report.status
    );
}

#[tokio::test]
async fn a_tool_whose_declared_capability_hides_behind_its_name_is_refused() {
    // Named like a write, declared as network access the harness never
    // granted: the call is held to what the tool really is.
    let case = format!(
        r#"
input: {{}}
{PATCHING_MODEL}
tools: {{ write_patch: {{ capability: NetworkAccess, responses: [ {{ ok: "x" }} ] }} }}
expect: {{ status: completed }}
"#
    );
    let report = only_case(&[("harness.yaml", REPAIR), ("cases/spoof.yaml", &case)]).await;
    assert_ne!(report.status, CaseStatus::Passed);
    let run = report.run.expect("the run itself happened");
    assert!(
        run.tools_invoked.is_empty(),
        "the spoofed tool never ran: {:?}",
        run.tools_invoked
    );
}

#[tokio::test]
async fn a_model_file_fixture_is_read_from_inside_the_harness_directory() {
    let case = r#"
input: { ticket: "x" }
model: { file: fixtures/model.yaml }
expect: { status: completed, output: { queue: bug-queue, label: bug } }
"#;
    let model = "sequence:\n  - text: '{\"label\": \"bug\"}'\n";
    let report = only_case(&[
        ("harness.yaml", ROUTING),
        ("fixtures/model.yaml", model),
        ("cases/file.yaml", case),
    ])
    .await;
    assert_eq!(
        report.status,
        CaseStatus::Passed,
        "{:?} {:?}",
        report.status,
        report.mismatches
    );

    let escaping = case.replace("fixtures/model.yaml", "../model.yaml");
    let report = only_case(&[("harness.yaml", ROUTING), ("cases/file.yaml", &escaping)]).await;
    assert!(
        matches!(report.status, CaseStatus::Error { .. }),
        "{:?}",
        report.status
    );
}

#[tokio::test]
async fn a_case_that_does_not_parse_is_an_error_not_a_panic() {
    let report = only_case(&[
        ("harness.yaml", ROUTING),
        ("cases/bad.yaml", "input: {}\nbogus: 1\n"),
    ])
    .await;
    assert!(
        matches!(report.status, CaseStatus::Error { .. }),
        "{:?}",
        report.status
    );
    assert!(report.run.is_none());
}

#[tokio::test]
async fn the_replay_check_can_be_turned_off_per_run() {
    let (_tmp, dir) = harness_dir(&[
        ("harness.yaml", ROUTING),
        ("cases/bug.yaml", ROUTING_BUG_CASE),
    ]);
    let options = RunnerOptions {
        replay_check: false,
        ..RunnerOptions::default()
    };
    let report = run_case(&dir, &dir.cases[0], &options).await;
    assert_eq!(report.status, CaseStatus::Passed);
    assert!(!report.replay_checked);
}

#[tokio::test]
async fn a_suite_counts_its_cases_and_honours_the_case_filter() {
    let (_tmp, dir) = harness_dir(&[
        ("harness.yaml", ROUTING),
        ("cases/bug.yaml", ROUTING_BUG_CASE),
        ("cases/feature.yaml", ROUTING_FEATURE_CASE),
        ("cases/unroutable.yaml", ROUTING_UNROUTABLE_CASE),
    ]);
    let report = run_suite(&dir, &opts()).await;
    assert!(report.valid, "{:?}", report.error);
    assert_eq!(
        (report.passed, report.failed, report.skipped),
        (3, 0, 0),
        "{:?}",
        report.cases
    );

    let options = RunnerOptions {
        case_filter: Some("feature".into()),
        ..RunnerOptions::default()
    };
    let report = run_suite(&dir, &options).await;
    assert_eq!(report.cases.len(), 1);
    assert_eq!(report.cases[0].case, "feature");
}

#[tokio::test]
async fn a_suite_over_an_invalid_harness_runs_nothing_and_says_why() {
    let (_tmp, dir) = harness_dir(&[
        (
            "harness.yaml",
            "api_version: \"2\"\nname: broken\nworkflow: []\n",
        ),
        ("cases/bug.yaml", ROUTING_BUG_CASE),
    ]);
    let report = run_suite(&dir, &opts()).await;
    assert!(!report.valid);
    assert!(report.error.is_some());
    assert!(report.cases.is_empty());
}

#[tokio::test]
async fn run_once_resume_once_and_replay_once_compose_without_a_case() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", REPORT)]);
    let blocked = RunRequest {
        input: json!({}),
        tools: serde_yaml::from_str(
            "shell_gate: { capability: ExecuteShell, responses: [ { block: true } ] }",
        )
        .unwrap(),
        bounds: serde_yaml::from_str("{ deadline_ms: 300 }").unwrap(),
        record: true,
        ..RunRequest::default()
    };
    let first = run_once(&dir, blocked)
        .await
        .expect("the first episode runs");
    assert_eq!(first.summary.status, RunStatus::Suspended);
    let checkpoint = first
        .checkpoint
        .clone()
        .expect("a bound leaves a checkpoint");

    let again = RunRequest {
        tools: serde_yaml::from_str(
            "shell_gate: { capability: ExecuteShell, responses: [ { ok: { exit_code: 0, timed_out: false, error: null, stdout: \"the report\", stderr: \"\" } } ] }",
        )
        .unwrap(),
        model: Some(ModelFixture::Fail("unused".into())),
        record: true,
        ..RunRequest::default()
    };
    let second = resume_once(&dir, checkpoint, again)
        .await
        .expect("the checkpoint resumes");
    assert_eq!(second.summary.status, RunStatus::Completed);
    assert_eq!(second.summary.output, Some(json!({"report": "the report"})));

    let recording = second.recording.expect("recording was asked for");
    let replay = replay_once(&dir, &recording).expect("a recording replays offline");
    assert_eq!(replay.output, second.summary.output);
    assert_eq!(replay.step_counts, second.summary.step_counts);
}

#[tokio::test]
async fn a_run_without_a_model_fixture_fails_a_model_step_cleanly() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", TWO_TURNS)]);
    let result = run_once(
        &dir,
        RunRequest {
            input: json!({"ticket": "x"}),
            ..RunRequest::default()
        },
    )
    .await
    .expect("the run itself ends");
    assert_eq!(result.summary.status, RunStatus::Failed);
    assert!(result
        .summary
        .failure
        .unwrap()
        .message
        .contains("no model fixture"));
}

#[tokio::test]
async fn inputs_that_miss_the_declared_contract_are_refused_before_running() {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", ROUTING)]);
    let error = run_once(
        &dir,
        RunRequest {
            input: json!({}),
            ..RunRequest::default()
        },
    )
    .await
    .expect_err("a missing input is refused");
    assert!(error.to_string().contains("ticket"), "{error}");
}
