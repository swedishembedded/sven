// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: one engine serves many harness runs at once, each in a workspace the
//! caller names for that run: the run's gates are rooted there and nowhere
//! else, and a path that is not a directory is refused up front.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{Engine, RunOptions};

const WORKFLOW: &str = r#"
api_version: "2"
name: located
execution: { limits: { model_turns: 4 } }
gates:
  marker: { command: [sh, -c, "test -f marker && pwd"], timeout: 20 }
workflow:
  - name: work
    task: Work.
    verify: [marker]
outputs:
  done: { type: string, value: "{{ tasks.work.output }}" }
"#;

async fn run_in(harness: &sven_sdk::Harness, dir: &std::path::Path) -> sven_sdk::HarnessOutcome {
    let mut run = harness
        .start(json!({}))
        .expect("starts")
        .in_workspace(dir)
        .expect("a directory");
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

#[tokio::test]
async fn concurrent_runs_on_one_engine_each_work_in_their_own_tree() {
    // An engine with no project root of its own.
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::always_text("done")) as Arc<_>)
        .build()
        .expect("an engine builds");
    let with_marker = tempfile::tempdir().unwrap();
    std::fs::write(with_marker.path().join("marker"), "x").unwrap();
    let without = tempfile::tempdir().unwrap();

    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let (a, b) = tokio::join!(
        run_in(&harness, with_marker.path()),
        run_in(&harness, without.path())
    );
    assert!(matches!(&a.result, Some(Ok(_))), "{:?}", a.result);
    let Some(Err(failure)) = &b.result else {
        panic!("the tree without the marker fails its gate: {:?}", b.result);
    };
    assert_eq!(failure.code, "gate_failed");
}

#[tokio::test]
async fn a_workspace_that_is_not_a_directory_is_refused_before_the_run() {
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::always_text("done")) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WORKFLOW).expect("loads");
    let run = harness.start(json!({})).expect("starts");
    let refused = run.in_workspace("/no/such/directory/anywhere").err();
    assert!(refused.is_some_and(|e| e.to_string().contains("not usable")));
}
