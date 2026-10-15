// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The demo runs to its stated success condition: each gate reaches the
//! session that raised it, both parked sessions survive the restart and are
//! decided from their stored state, and only the approved services deploy.

use sample_host_sessions::{demo_config, run, Options};
use sven_sdk::{Decision, RunConclusion};

#[tokio::test]
async fn the_demo_routes_parks_restarts_and_decides() {
    let dir = tempfile::tempdir().unwrap();
    let report = run(&Options {
        workspace: dir.path().to_path_buf(),
        config: demo_config(),
    })
    .await
    .expect("the demo runs");

    assert_eq!(report.verified, Ok(()));
    assert_eq!(report.deployed, ["ledger", "search"]);
    assert!(report.numbering_continued);
    assert_eq!(
        report
            .decided
            .iter()
            .map(|d| (d.service.as_str(), d.decision.clone()))
            .collect::<Vec<_>>(),
        [
            ("search", Decision::Approve),
            (
                "billing",
                Decision::Deny {
                    reason: "billing is frozen until the audit ends".into()
                }
            ),
        ]
    );
    assert!(report
        .decided
        .iter()
        .all(|d| d.concluded == RunConclusion::Success));
}
