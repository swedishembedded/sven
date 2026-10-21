// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: expectations are judged on observable behavior, and a mismatch says
//! where in the value the two sides part (JSON pointers), so a failed case
//! can be read without a debugger.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod common;

use common::*;
use serde_json::json;
use sven_harness_testkit::{includes, json_diff, run_case, CaseStatus, RunnerOptions};

#[test]
fn equal_values_have_no_diff() {
    let value = json!({"a": [1, {"b": null}], "c": "x"});
    assert!(json_diff(&value, &value).is_empty());
}

#[test]
fn a_diff_points_at_every_difference() {
    let expected = json!({"queue": "bugs", "tags": ["a", "b"], "meta": {"n": 1}});
    let actual = json!({"queue": "backlog", "tags": ["a", "c", "d"], "meta": {}, "extra": true});
    let diff = json_diff(&expected, &actual);
    let by_pointer = |p: &str| {
        diff.iter()
            .find(|d| d.pointer == p)
            .unwrap_or_else(|| panic!("no diff at {p}: {diff:?}"))
    };
    assert_eq!(by_pointer("/queue").expected, Some(json!("bugs")));
    assert_eq!(by_pointer("/queue").actual, Some(json!("backlog")));
    assert_eq!(by_pointer("/tags/1").actual, Some(json!("c")));
    assert_eq!(
        by_pointer("/tags/2").expected,
        None,
        "an extra element has no expectation"
    );
    assert_eq!(
        by_pointer("/meta/n").actual,
        None,
        "a missing key has no actual"
    );
    assert_eq!(by_pointer("/extra").actual, Some(json!(true)));
    assert_eq!(diff.len(), 5);
}

#[test]
fn a_type_change_is_one_difference_at_the_parent() {
    let diff = json_diff(&json!({"a": {"b": 1}}), &json!({"a": [1]}));
    assert_eq!(diff.len(), 1);
    assert_eq!(diff[0].pointer, "/a");
}

#[test]
fn pointer_segments_are_escaped() {
    let diff = json_diff(&json!({"a/b": 1, "c~d": 1}), &json!({"a/b": 2, "c~d": 2}));
    let pointers: Vec<&str> = diff.iter().map(|d| d.pointer.as_str()).collect();
    assert_eq!(pointers, ["/a~1b", "/c~0d"]);
}

#[test]
fn includes_accepts_extra_keys_but_not_missing_or_different_ones() {
    let actual = json!({"queue": "bugs", "label": "bug", "nested": {"x": 1, "y": 2}});
    assert!(includes(&json!({"queue": "bugs"}), &actual).is_empty());
    assert!(includes(&json!({"nested": {"y": 2}}), &actual).is_empty());

    let diff = includes(
        &json!({"queue": "backlog", "missing": 1, "nested": {"x": 9}}),
        &actual,
    );
    let pointers: Vec<&str> = diff.iter().map(|d| d.pointer.as_str()).collect();
    assert_eq!(pointers, ["/missing", "/nested/x", "/queue"]);
}

#[test]
fn includes_compares_arrays_exactly() {
    assert!(includes(&json!({"a": [1, 2]}), &json!({"a": [1, 2], "b": 1})).is_empty());
    assert!(!includes(&json!({"a": [1]}), &json!({"a": [1, 2]})).is_empty());
}

async fn judge(case: &str) -> sven_harness_testkit::CaseReport {
    let (_tmp, dir) = harness_dir(&[("harness.yaml", REPAIR), ("cases/c.yaml", case)]);
    run_case(&dir, &dir.cases[0], &RunnerOptions::default()).await
}

const WRITES_OK: &str = r#"
input: {}
model:
  sequence:
    - tool_calls: [ { id: p1, name: write_patch, args: {} } ]
    - text: done
tools: { write_patch: { capability: WriteFile, responses: [ { ok: "p", write: { path: fixed.txt, content: x } } ] } }
"#;

#[tokio::test]
async fn tool_and_capability_expectations_are_judged_on_executed_calls() {
    let passing = format!("{WRITES_OK}expect:\n  status: completed\n  tools_invoked: [write_patch]\n  tools_not_invoked: [read_notes]\n  forbidden_capabilities: [ExecuteShell]\n  max_steps: 5\n");
    let report = judge(&passing).await;
    assert_eq!(report.status, CaseStatus::Passed, "{:?}", report.mismatches);

    let failing = format!("{WRITES_OK}expect:\n  status: completed\n  tools_invoked: []\n  tools_not_invoked: [write_patch]\n  forbidden_capabilities: [WriteFile]\n  max_steps: 1\n");
    let report = judge(&failing).await;
    assert_eq!(report.status, CaseStatus::Failed);
    let names: Vec<&str> = report
        .mismatches
        .iter()
        .map(|m| m.expectation.as_str())
        .collect();
    for expected in [
        "tools_invoked",
        "tools_not_invoked",
        "forbidden_capabilities",
        "max_steps",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
}

#[tokio::test]
async fn output_includes_is_a_subset_check_and_output_is_exact() {
    let routing = |expect: &str| {
        format!("input: {{ ticket: t }}\nmodel: {{ sequence: [ {{ text: '{{\"label\": \"bug\"}}' }} ] }}\nexpect:\n  status: completed\n{expect}")
    };
    let run = |expect: String| async move {
        let (_tmp, dir) = harness_dir(&[
            ("harness.yaml", ROUTING),
            ("cases/c.yaml", &routing(&expect)),
        ]);
        run_case(&dir, &dir.cases[0], &RunnerOptions::default()).await
    };
    assert_eq!(
        run("  output_includes: { queue: bug-queue }\n".into())
            .await
            .status,
        CaseStatus::Passed
    );
    let exact = run("  output: { queue: bug-queue }\n".into()).await;
    assert_eq!(
        exact.status,
        CaseStatus::Failed,
        "output is exact, so the extra label fails it"
    );
    let mismatch = &exact.mismatches[0];
    assert_eq!(mismatch.expectation, "output");
    assert_eq!(mismatch.diff[0].pointer, "/label");
}
