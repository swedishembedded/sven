// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a session starts from a recorded transcript and a workspace
//! snapshot, and continues the same tool loop on another model.
//!
//! The fork replays the recorded actions into a fresh workspace and is
//! refused unless every replayed observation is the one that was recorded,
//! the workspace it leaves is the one recorded at that step, and the
//! visible transcript hashes to what was recorded. A refused fork performs
//! no further work: the caller learns which of the three disagreed.
//!
//! Swedish Embedded AB implements agent runtimes that can continue a
//! recorded session from another model, for its clients. If your team needs
//! expertise in on-policy agent data then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;
use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::fork::{workspace_digest, Fork, ForkError, RecordedState};
use sven_sdk::{Agent, Engine, RunOptions, Toolset, Turn};

/// One scripted assistant reply.
fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

/// One scripted tool call.
fn calls(id: &str, name: &str, arguments: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
        ResponseEvent::Done,
    ]
}

fn engine(root: &Path, provider: Arc<ScriptedMockProvider>) -> Engine {
    Engine::builder()
        .model_provider(provider as Arc<_>)
        .toolset(Toolset::coding())
        .project_root(root)
        .build()
        .expect("an engine builds")
}

/// A tree with one file, as both the student and the fork start it.
fn base(dir: &Path, text: &str) {
    std::fs::write(dir.join("input.txt"), text).expect("the base file is written");
}

/// A student run that reads the base file and stops.
async fn student_run(tree: &Path) -> (Vec<Turn>, RecordedState) {
    let provider = Arc::new(ScriptedMockProvider::new(vec![
        calls("c1", "read_file", r#"{"path":"input.txt"}"#),
        says("read it"),
    ]));
    let mut agent = engine(tree, provider).agent("agent");
    agent
        .send("read the note")
        .await
        .expect("the student's turn completes");
    let transcript = agent.state().transcript();
    let recorded = RecordedState::of(&transcript, tree).expect("the recording is made");
    (transcript, recorded)
}

/// The messages the model was shown, without the system prompt sven seeds.
fn shown(agent: &Agent) -> Vec<Turn> {
    agent
        .state()
        .transcript()
        .into_iter()
        .filter(|turn| !matches!(turn, Turn::System { .. }))
        .collect()
}

#[tokio::test]
async fn a_fork_replays_the_recorded_actions_and_continues_the_tool_loop_on_another_model() {
    let student_tree = tempfile::tempdir().expect("a student tree");
    base(student_tree.path(), "alpha");
    let (transcript, recorded) = student_run(student_tree.path()).await;

    // The fork starts from the same base state, in a tree of its own.
    let fork_tree = tempfile::tempdir().expect("a fork tree");
    base(fork_tree.path(), "alpha");

    let teacher = Arc::new(ScriptedMockProvider::new(vec![
        calls("t1", "write_file", r#"{"path":"fixed.txt","text":"done"}"#),
        says("corrected"),
    ]));
    let mut forked = engine(fork_tree.path(), Arc::clone(&teacher))
        .fork(&Fork::new(transcript.clone(), fork_tree.path(), recorded))
        .await
        .expect("the replay matches and the fork is admitted");

    // The fork starts exactly where the student stopped: the same visible
    // turns, and nothing added.
    assert_eq!(shown(&forked), transcript);

    forked
        .continue_with(RunOptions::new())
        .await
        .expect("the teacher's turn completes");

    // The teacher continued the same tool loop: its call ran in the fork
    // workspace, and its turn is appended to the student's transcript.
    assert_eq!(
        std::fs::read_to_string(fork_tree.path().join("fixed.txt")).expect("the teacher's file"),
        "done"
    );
    let after = shown(&forked);
    assert_eq!(&after[..transcript.len()], transcript.as_slice());
    assert_eq!(
        after.len(),
        transcript.len() + 3,
        "call, result, then reply: {after:?}"
    );
    assert_eq!(
        after.last(),
        Some(&Turn::Assistant {
            text: "corrected".into(),
            tool_calls: Vec::new()
        })
    );

    // The teacher saw the student's conversation, with no message invented
    // between them: a continuation, not a new prompt.
    let request = teacher
        .last_request
        .lock()
        .unwrap()
        .clone()
        .expect("the teacher was asked");
    let seen: Vec<_> = request
        .messages
        .iter()
        .filter(|message| message.role != sven_model::Role::System)
        .collect();
    assert!(
        seen.len() >= transcript.len(),
        "the recorded conversation leads the continuation: {seen:?}"
    );
    assert_eq!(seen[0].as_text(), Some("read the note"));
    assert!(
        seen[1..]
            .iter()
            .all(|message| message.role != sven_model::Role::User),
        "a continuation invents no user turn: {seen:?}"
    );
    match &seen[1].content {
        sven_model::MessageContent::ToolCall { function, .. } => {
            assert_eq!(function.name, "read_file");
            assert_eq!(function.arguments, r#"{"path":"input.txt"}"#);
        }
        other => panic!("the recorded call is offered back, got {other:?}"),
    }
    let recorded_result = transcript
        .iter()
        .find_map(|turn| match turn {
            Turn::ToolResult { text, .. } => Some(text),
            _ => None,
        })
        .expect("the student observed the file");
    match &seen[2].content {
        sven_model::MessageContent::ToolResult { content, .. } => {
            assert_eq!(content.as_text(), Some(recorded_result.as_str()));
        }
        other => panic!("the recorded observation is offered back, got {other:?}"),
    }
    assert_eq!(seen[3].as_text(), Some("read it"));
    assert_eq!(
        seen.len(),
        transcript.len() + 2,
        "the teacher's call and its result follow the prefix: {seen:?}"
    );
}

#[tokio::test]
async fn a_fork_is_refused_when_a_replayed_observation_differs_from_the_recorded_one() {
    let student_tree = tempfile::tempdir().expect("a student tree");
    base(student_tree.path(), "alpha");
    let (transcript, recorded) = student_run(student_tree.path()).await;

    let fork_tree = tempfile::tempdir().expect("a fork tree");
    base(fork_tree.path(), "beta");

    let teacher = Arc::new(ScriptedMockProvider::new(vec![says("unused")]));
    let refused = engine(fork_tree.path(), teacher)
        .fork(&Fork::new(transcript, fork_tree.path(), recorded))
        .await;
    match refused {
        Err(ForkError::ResultMismatch { call_id, .. }) => assert_eq!(call_id, "c1"),
        Err(other) => panic!("the replay was refused for another reason: {other}"),
        Ok(_) => panic!("the replay differed but the fork was admitted"),
    }
}

#[tokio::test]
async fn a_fork_is_refused_when_the_replayed_workspace_is_not_the_recorded_one() {
    let student_tree = tempfile::tempdir().expect("a student tree");
    base(student_tree.path(), "alpha");
    let (transcript, recorded) = student_run(student_tree.path()).await;

    let fork_tree = tempfile::tempdir().expect("a fork tree");
    base(fork_tree.path(), "alpha");
    let elsewhere = tempfile::tempdir().expect("another tree");
    base(elsewhere.path(), "gamma");
    let wrong = RecordedState::new(
        workspace_digest(elsewhere.path()).expect("a digest"),
        recorded.transcript.clone(),
    );

    let teacher = Arc::new(ScriptedMockProvider::new(vec![says("unused")]));
    let refused = engine(fork_tree.path(), teacher)
        .fork(&Fork::new(transcript, fork_tree.path(), wrong))
        .await;
    match refused {
        Err(ForkError::WorkspaceMismatch { .. }) => {}
        Err(other) => panic!("the workspace was refused for another reason: {other}"),
        Ok(_) => panic!("the workspace differed but the fork was admitted"),
    }
}

#[tokio::test]
async fn a_fork_is_refused_before_replaying_when_the_transcript_hash_differs() {
    let student_tree = tempfile::tempdir().expect("a student tree");
    base(student_tree.path(), "alpha");
    let (transcript, recorded) = student_run(student_tree.path()).await;

    let fork_tree = tempfile::tempdir().expect("a fork tree");
    base(fork_tree.path(), "alpha");
    let wrong = RecordedState::new(recorded.workspace.clone(), "sha256:0000".to_string());

    let teacher = Arc::new(ScriptedMockProvider::new(vec![says("unused")]));
    let refused = engine(fork_tree.path(), teacher)
        .fork(&Fork::new(transcript, fork_tree.path(), wrong))
        .await;
    match refused {
        Err(ForkError::TranscriptMismatch { .. }) => {}
        Err(other) => panic!("the transcript was refused for another reason: {other}"),
        Ok(_) => panic!("a transcript that is not the recorded one was admitted"),
    }
    assert!(
        !fork_tree.path().join(".sven").exists(),
        "a refused fork does not replay anything into the workspace"
    );
}

#[test]
fn a_call_with_no_recorded_result_cannot_be_forked() {
    let mut transcript = vec![Turn::User {
        text: "read it".into(),
    }];
    transcript.push(Turn::Assistant {
        text: String::new(),
        tool_calls: vec![sven_sdk::ToolCallRecord {
            id: "c1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"input.txt"}"#.into(),
        }],
    });
    let fork = Fork::new(
        transcript,
        Path::new("."),
        RecordedState::new("sha256:00", "sha256:00"),
    );
    assert!(
        matches!(fork.calls(), Err(ForkError::UnansweredCall { call_id }) if call_id == "c1"),
        "an action with no observation is not a recorded step"
    );
}

#[test]
fn a_workspace_digest_covers_paths_and_contents_and_ignores_repository_state() {
    let first = tempfile::tempdir().expect("a tree");
    let second = tempfile::tempdir().expect("another tree");
    base(first.path(), "alpha");
    base(second.path(), "alpha");
    assert_eq!(
        workspace_digest(first.path()).expect("a digest"),
        workspace_digest(second.path()).expect("a digest"),
        "the same tree has the same digest"
    );

    std::fs::write(second.path().join("input.txt"), "alpha\n").expect("a changed file");
    assert_ne!(
        workspace_digest(first.path()).expect("a digest"),
        workspace_digest(second.path()).expect("a digest"),
        "a changed file changes the digest"
    );
    std::fs::write(second.path().join("input.txt"), "alpha").expect("the file is restored");

    std::fs::create_dir_all(first.path().join(".git/objects")).expect("repository state");
    std::fs::write(first.path().join(".git/objects/x"), "noise").expect("repository state");
    std::fs::create_dir_all(first.path().join("__pycache__")).expect("interpreter state");
    std::fs::write(first.path().join("__pycache__/m.pyc"), "noise").expect("interpreter state");
    assert_eq!(
        workspace_digest(first.path()).expect("a digest"),
        workspace_digest(second.path()).expect("a digest"),
        "repository and interpreter bookkeeping is not the workspace the model edits"
    );
}
