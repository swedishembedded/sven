// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `repo-repair` harness on a real repository with the real tools: the
//! shell runs the tests, `read_file` and `edit_file` change the files, and
//! the model is scripted for determinism. What is judged is the repository
//! on disk, not the model's claims.
//!
//! Swedish Embedded AB implements verifiable automated code-repair workflows
//! for its clients. If your team needs expertise in agent harnesses for
//! fixing failing builds then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sample_harnesses_sdk_runner::{repair, Answer, SequencedModel};
use serde_json::json;

fn harness_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../repo-repair")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A fresh copy of the fixture repository; the tests fail in it.
fn failing_repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().canonicalize().unwrap().join("repo");
    copy_tree(&harness_dir().join("fixtures/repo"), &repo);
    (tmp, repo)
}

fn tests_pass(repo: &Path) -> bool {
    Command::new("sh")
        .arg("tests/test_total.sh")
        .current_dir(repo)
        .output()
        .unwrap()
        .status
        .success()
}

const DIAGNOSIS: &str =
    r#"{"file": "src/total.sh", "summary": "total ignores the shipping argument"}"#;

/// A diff that applies but leaves the discount unhandled.
const SHIPPING_ONLY: &str =
    "@@ -1,1 +1,1 @@\n-  echo $(( items - discount ))\n+  echo $(( items + shipping ))\n";
/// A diff from the first attempt's result to the right total.
const SHIPPING_AND_DISCOUNT: &str =
    "@@ -1,1 +1,1 @@\n-  echo $(( items + shipping ))\n+  echo $(( items + shipping - discount ))\n";
/// A diff that fixes the original file in one go.
const RIGHT_AT_ONCE: &str =
    "@@ -1,1 +1,1 @@\n-  echo $(( items - discount ))\n+  echo $(( items + shipping - discount ))\n";

fn edit(id: &str, diff: &str) -> Answer {
    Answer::call(
        id,
        "edit_file",
        json!({"path": "src/total.sh", "diff": diff}),
    )
}

#[tokio::test]
async fn a_wrong_first_edit_is_retried_with_the_new_failure_and_the_repository_ends_green() {
    let (_tmp, repo) = failing_repo();
    assert!(!tests_pass(&repo), "the fixture starts red");
    let model = SequencedModel::new([
        Answer::text(DIAGNOSIS),
        edit("e1", SHIPPING_ONLY),
        Answer::text("edited"),
        edit("e2", SHIPPING_AND_DISCOUNT),
        Answer::text("edited again"),
    ]);

    let report = repair(&harness_dir(), &repo, Arc::new(model))
        .await
        .unwrap();

    assert_eq!(report.conclusion, "success", "{report:?}");
    assert_eq!(
        report.output,
        Some(json!({"status": "fixed", "attempts": 2}))
    );
    assert_eq!(report.steps.get("fix_first"), Some(&1));
    assert_eq!(report.steps.get("fix_second"), Some(&1));
    assert!(tests_pass(&repo), "the repository really is green");
}

#[tokio::test]
async fn a_right_first_edit_takes_one_attempt_and_never_runs_the_retry() {
    let (_tmp, repo) = failing_repo();
    let model = SequencedModel::new([
        Answer::text(DIAGNOSIS),
        edit("e1", RIGHT_AT_ONCE),
        Answer::text("edited"),
    ]);
    let report = repair(&harness_dir(), &repo, Arc::new(model))
        .await
        .unwrap();
    assert_eq!(
        report.output,
        Some(json!({"status": "fixed", "attempts": 1}))
    );
    assert_eq!(report.steps.get("fix_second"), None);
    assert!(tests_pass(&repo));
}

#[tokio::test]
async fn an_agent_claiming_success_without_a_fix_fails_the_run_and_the_repository_stays_red() {
    let (_tmp, repo) = failing_repo();
    let model = SequencedModel::new([
        Answer::text(DIAGNOSIS),
        Answer::text("I fixed it, trust me"),
        Answer::text("I fixed it again"),
    ]);
    let report = repair(&harness_dir(), &repo, Arc::new(model))
        .await
        .unwrap();
    assert_eq!(
        report.failure_code.as_deref(),
        Some("unrepaired"),
        "{report:?}"
    );
    assert!(!tests_pass(&repo));
}

#[tokio::test]
async fn an_already_green_repository_is_left_alone() {
    let (_tmp, repo) = failing_repo();
    std::fs::write(
        repo.join("src/total.sh"),
        "total() {\n  echo $(( $1 + $2 - $3 ))\n}\n",
    )
    .unwrap();
    let report = repair(&harness_dir(), &repo, Arc::new(SequencedModel::new([])))
        .await
        .unwrap();
    assert_eq!(
        report.output,
        Some(json!({"status": "already_green", "attempts": 0}))
    );
    assert_eq!(report.output_tokens, 0, "no model call was needed");
}
