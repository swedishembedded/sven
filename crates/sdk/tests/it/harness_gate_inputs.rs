// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: what a gate may be given beyond the workspace. `{{ run.trajectory_file }}`
//! is the run's trajectory so far, written by the host outside the workspace
//! just before the command starts; `cwd_input` runs the command in a directory
//! the host named and the agent cannot write. The corrections a supervisor sent
//! and the messages its accept gate refused are readable after the run.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in checks that judge what an
//! agent did and not only what it left behind then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, HarnessRun, RunOptions};

fn engine(
    reply: impl Fn(&sven_model::CompletionRequest) -> String + Send + Sync + 'static,
) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |req| {
            vec![ResponseEvent::TextDelta(reply(req)), ResponseEvent::Done]
        })) as Arc<_>)
        .build()
        .expect("an engine builds")
}

async fn finish(run: &mut HarnessRun) -> Result<Value, (String, String)> {
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    match outcome.result {
        Some(Ok(value)) => Ok(value),
        Some(Err(f)) => Err((f.code, f.message)),
        None => panic!("no verdict"),
    }
}

const JUDGED: &str = r#"
api_version: "2"
name: judged
execution: { limits: { model_turns: 4 } }
gates:
  saw_task:
    command: [sh, -c, 'grep -q "Do the thing" "$1"', sh, "{{ run.trajectory_file }}"]
    timeout: 20
workflow:
  - name: work
    task: Do the thing.
    verify: [saw_task]
"#;

#[tokio::test]
async fn a_gate_reads_the_trajectory_so_far_from_a_file_outside_the_workspace() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = engine(|_| "done".into())
        .load_harness_from_str(JUDGED)
        .expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    assert!(
        finish(&mut run).await.is_ok(),
        "the gate found the task in the trajectory"
    );

    let wrong = JUDGED.replace("Do the thing\"", "Never said\"");
    let harness = engine(|_| "done".into())
        .load_harness_from_str(&wrong.replace("name: judged", "name: judged2"))
        .expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    let (code, _) = finish(&mut run)
        .await
        .expect_err("a gate that does not find it fails");
    assert_eq!(code, "gate_failed");
}

fn trajectory_dir(id: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sven-harness-{id}"))
}

#[tokio::test]
async fn a_host_that_asks_to_keep_it_finds_the_final_trajectory_in_the_file_after_the_run() {
    let workspace = tempfile::tempdir().unwrap();
    let harness = engine(|_| "all done".into())
        .load_harness_from_str(JUDGED)
        .expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap()
        .keep_trajectory_file();
    let id = run.run_id().to_string();
    finish(&mut run).await.expect("completes");
    let file = trajectory_dir(&id).join("trajectory.json");
    assert!(!file.starts_with(workspace.path()));
    let text = std::fs::read_to_string(&file).expect("the file is there");
    assert!(text.contains("all done"), "the final answer is in it");
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[tokio::test]
async fn the_file_is_removed_when_the_run_ends_whether_it_succeeded_or_failed() {
    let workspace = tempfile::tempdir().unwrap();
    for (name, document, succeeds) in [
        ("kept_none_ok", JUDGED.to_string(), true),
        (
            "kept_none_failed",
            JUDGED
                .replace("Do the thing\"", "Never said\"")
                .replace("name: judged", "name: kept_none_failed"),
            false,
        ),
    ] {
        let harness = engine(|_| "all done".into())
            .load_harness_from_str(&document.replace("name: judged", &format!("name: {name}")))
            .expect("loads");
        let mut run = harness
            .start(json!({}))
            .unwrap()
            .in_workspace(workspace.path())
            .unwrap();
        let id = run.run_id().to_string();
        assert_eq!(finish(&mut run).await.is_ok(), succeeds, "{name}");
        assert!(
            !trajectory_dir(&id).join("trajectory.json").exists(),
            "{name}: the trajectory file outlived the run"
        );
        let _ = std::fs::remove_dir_all(trajectory_dir(&id));
    }
}

const PINNED: &str = r#"
api_version: "2"
name: pinned
inputs: { pin: string }
execution: { limits: { model_turns: 4 } }
gates:
  pinned_file:
    command: [test, -f, pinned.txt]
    cwd_input: pin
    timeout: 20
workflow:
  - name: work
    task: Work.
    verify: [pinned_file]
"#;

#[tokio::test]
async fn a_gate_runs_in_a_directory_the_host_named_and_never_in_one_the_agent_can_write() {
    let workspace = tempfile::tempdir().unwrap();
    let pin = tempfile::tempdir().unwrap();
    std::fs::write(pin.path().join("pinned.txt"), "x").unwrap();
    let harness = engine(|_| "done".into())
        .load_harness_from_str(PINNED)
        .expect("loads");
    let mut run = harness
        .start(json!({ "pin": pin.path() }))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    assert!(
        finish(&mut run).await.is_ok(),
        "the file in the pinned directory is found"
    );

    // The workspace itself is not a trusted directory: the agent writes there.
    std::fs::write(workspace.path().join("pinned.txt"), "x").unwrap();
    let mut run = harness
        .start(json!({ "pin": workspace.path() }))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    let (code, _) = finish(&mut run).await.expect_err("refused");
    assert_eq!(code, "gate_failed");
}

const SUPERVISED: &str = r#"
api_version: "2"
name: supervised
execution: { limits: { model_turns: 30 } }
gates:
  check:
    command: [sh, -c, 'test -f fixed.txt || { touch fixed.txt; exit 1; }']
    timeout: 20
  accept:
    command: [grep, -q, fixed]
    stdin: "{{ tasks.supervisor.output }}"
    timeout: 20
workflow:
  - name: work
    task: Make it work.
    corrections:
      max: 2
      check: check
      author:
        name: supervisor
        system: You are the supervisor.
        task: "Write the follow-up."
      accept: accept
      retries: 1
"#;

#[tokio::test]
async fn corrections_and_refused_messages_are_readable_after_the_run() {
    let drafts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&drafts);
    let engine = engine(move |req| {
        let supervisor = req.messages.iter().any(|m| {
            m.as_text()
                .is_some_and(|t| t.contains("You are the supervisor"))
        });
        if !supervisor {
            return "ok".into();
        }
        // The first draft lacks the word the accept gate wants.
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            "bad draft".into()
        } else {
            "it is fixed now, recheck".into()
        }
    });
    let workspace = tempfile::tempdir().unwrap();
    let harness = engine.load_harness_from_str(SUPERVISED).expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    finish(&mut run).await.expect("the run completes");

    let corrections = run.corrections().expect("after the run");
    assert_eq!(corrections.len(), 1, "{corrections:?}");
    assert_eq!(corrections[0].message, "it is fixed now, recheck");
    assert_eq!(corrections[0].tool_calls_before, 0);
    let refused = run.refused_messages().expect("after the run");
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0].author, "supervisor");
    assert_eq!(refused[0].message, "bad draft");
    assert!(!refused[0].reason.is_empty());
}

#[tokio::test]
async fn the_last_refused_message_is_listed_though_it_was_not_sent_back() {
    let engine = engine(|req| {
        let supervisor = req.messages.iter().any(|m| {
            m.as_text()
                .is_some_and(|t| t.contains("You are the supervisor"))
        });
        if supervisor {
            "never what the gate wants".into()
        } else {
            "ok".into()
        }
    });
    let workspace = tempfile::tempdir().unwrap();
    let harness = engine.load_harness_from_str(SUPERVISED).expect("loads");
    let mut run = harness
        .start(json!({}))
        .unwrap()
        .in_workspace(workspace.path())
        .unwrap();
    let _ = finish(&mut run).await;

    // `retries: 1`: the first refusal was sent back, the second was not.
    let refused = run.refused_messages().expect("after the run");
    assert_eq!(refused.len(), 2, "{refused:?}");
    assert!(refused
        .iter()
        .all(|r| r.message == "never what the gate wants"));
    assert!(refused.iter().all(|r| !r.reason.is_empty()));
    assert!(
        refused[1].reason.contains("status 1"),
        "the gate's own reason: {refused:?}"
    );
    assert!(run.corrections().expect("after the run").is_empty());
}
