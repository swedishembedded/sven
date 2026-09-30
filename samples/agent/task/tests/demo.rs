// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The demo runs to its stated success condition: the question parks the
//! run, the resumed agent finishes the draft, publishing is approved because
//! the draft passes the check, and the trajectory is valid ATIF.

use std::time::Duration;

use sample_agent_task::{demo_config, run, verify, Options, CHANGES, DEMO_CHANGES, PUBLISHED};
use sven_sdk::{CancelToken, RunConclusion};

#[tokio::test]
async fn the_demo_drafts_asks_publishes_and_passes_the_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(CHANGES), DEMO_CHANGES).unwrap();
    let report = run(&Options {
        workspace: dir.path().to_path_buf(),
        config: demo_config(),
        answer: "users".into(),
        cancel: CancelToken::new(),
        deadline: Duration::from_secs(60),
    })
    .await
    .expect("the demo runs");

    assert_eq!(report.drafted.conclusion, RunConclusion::Waiting);
    let answered = report.answered.expect("the question was answered");
    assert_eq!(answered.conclusion, RunConclusion::Success);
    assert_eq!(report.published.conclusion, RunConclusion::Success);
    assert!(dir.path().join(PUBLISHED).exists(), "publishing ran");
    assert_eq!(report.verified, Ok(()));
    assert_eq!(verify(dir.path()), Ok(()));

    let text = std::fs::read_to_string(&report.trajectory).unwrap();
    let trajectory: sven_sdk::atif::Trajectory = serde_json::from_str(&text).unwrap();
    sven_sdk::atif::validate_trajectory(&trajectory).expect("valid ATIF");
    for tool in ["read_changes", "write_notes", "ask_question", "publish"] {
        assert!(text.contains(tool), "{tool} is in the trajectory");
    }
}

#[tokio::test]
async fn a_draft_that_misses_a_change_fails_the_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(CHANGES),
        "- Faster start-up\n- Offline mode\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("RELEASE_NOTES.md"), "Faster start-up.").unwrap();
    let err = verify(dir.path()).expect_err("offline mode is missing");
    assert!(err.contains("Offline mode"), "{err}");
}
