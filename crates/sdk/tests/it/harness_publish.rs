// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a task that names `publish:` hands its host the files it left, each
//! with its digest, together with a checkpoint taken right after. Resuming
//! that checkpoint runs only the tasks after it, and a file the host replaced
//! before resuming is what those tasks find. A `file` input must name a file
//! in the workspace before anything runs; a published file that is not there
//! fails the run; and a `retry: never` task interrupted after it acted is not
//! resumed into.
//!
//! Swedish Embedded AB implements verifiable, resumable agent workflows for
//! its clients. If your team needs expertise in handing finished work from
//! an automated run to the system around it then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{CallError, CancelToken, Engine, Publication, RunOptions};

const DESIGN: &str = r#"
api_version: "2"
name: design
inputs: { drawing: file }
execution: { limits: { model_turns: 10 } }
workflow:
  - name: model
    task: "Model the part in {{ inputs.drawing }}."
    publish: [part.step]
  - name: review
    task: "Review part.step."
    publish: [part.step]
"#;

/// An engine whose model answers every request and counts them.
fn engine(
    requests: &Arc<AtomicUsize>,
    on_request: impl Fn(usize) + Send + Sync + 'static,
) -> Engine {
    let counter = Arc::clone(requests);
    Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |_| {
            let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
            on_request(n);
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done]
        })) as Arc<_>)
        .build()
        .expect("an engine builds")
}

fn workspace(part: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("drawing.png"), b"png").unwrap();
    std::fs::write(dir.path().join("part.step"), part).unwrap();
    dir
}

async fn finish(run: &mut sven_sdk::HarnessRun) -> sven_sdk::HarnessOutcome {
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome")
}

#[tokio::test]
async fn a_publication_hands_over_the_files_and_resumes_past_its_task() {
    let requests = Arc::new(AtomicUsize::new(0));
    let engine = engine(&requests, |_| {});
    let harness = engine.load_harness_from_str(DESIGN).expect("loads");
    let dir = workspace("v1");

    let published: Arc<Mutex<Vec<Publication>>> = Arc::default();
    let sink = Arc::clone(&published);
    let mut run = harness
        .start(json!({ "drawing": "drawing.png" }))
        .expect("starts")
        .in_workspace(dir.path())
        .expect("a workspace")
        .on_publish(move |publication| sink.lock().unwrap().push(publication));
    let outcome = finish(&mut run).await;
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2, "one turn per task");

    let taken = published.lock().unwrap().clone();
    assert_eq!(
        taken.iter().map(|p| p.task.as_str()).collect::<Vec<_>>(),
        ["model", "review"],
        "each publishing task hands over once, in order"
    );
    assert_eq!(taken[0].files.len(), 1);
    assert_eq!(taken[0].files[0].path, "part.step");
    let first_digest = taken[0].files[0].digest.clone();
    assert_eq!(
        taken[1].files[0].digest, first_digest,
        "nothing changed the file"
    );

    // Resume after the first publication with the file replaced: only the
    // task after it runs, and it finds the replacement.
    std::fs::write(dir.path().join("part.step"), "v2").unwrap();
    requests.store(0, Ordering::SeqCst);
    let again: Arc<Mutex<Vec<Publication>>> = Arc::default();
    let sink = Arc::clone(&again);
    let mut resumed = harness
        .resume(taken[0].checkpoint.clone())
        .expect("the checkpoint fits")
        .in_workspace(dir.path())
        .expect("a workspace")
        .on_publish(move |publication| sink.lock().unwrap().push(publication));
    let outcome = finish(&mut resumed).await;
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        1,
        "only `review` runs again"
    );
    let again = again.lock().unwrap().clone();
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].task, "review");
    assert_ne!(
        again[0].files[0].digest, first_digest,
        "the replacement is what it published"
    );
}

#[tokio::test]
async fn a_published_file_that_is_not_there_fails_the_run() {
    let requests = Arc::new(AtomicUsize::new(0));
    let engine = engine(&requests, |_| {});
    let harness = engine.load_harness_from_str(DESIGN).expect("loads");
    let dir = workspace("v1");
    std::fs::remove_file(dir.path().join("part.step")).unwrap();
    let mut run = harness
        .start(json!({ "drawing": "drawing.png" }))
        .expect("starts")
        .in_workspace(dir.path())
        .expect("a workspace");
    let outcome = finish(&mut run).await;
    let failure = format!("{:?}", outcome.result);
    assert!(failure.contains("publish_failed"), "{failure}");
    assert!(failure.contains("part.step"), "{failure}");
}

#[tokio::test]
async fn a_file_input_must_name_a_file_in_the_workspace() {
    let requests = Arc::new(AtomicUsize::new(0));
    let engine = engine(&requests, |_| {});
    let harness = engine.load_harness_from_str(DESIGN).expect("loads");
    assert!(
        matches!(
            harness.start(json!({ "drawing": "../etc/passwd" })),
            Err(CallError::Precondition(_))
        ),
        "a path out of the workspace is refused before anything starts"
    );
    let dir = workspace("v1");
    let mut run = harness
        .start(json!({ "drawing": "missing.png" }))
        .expect("the shape is fine")
        .in_workspace(dir.path())
        .expect("a workspace");
    let refused = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("returns");
    assert!(
        matches!(&refused, Err(CallError::Precondition(why)) if why.contains("missing.png")),
        "{refused:?}"
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        0,
        "nothing was sent to a model"
    );
}

const DEPLOY: &str = r#"
api_version: "2"
name: release
execution: { limits: { model_turns: 10 } }
workflow:
  - name: deploy
    retry: never
    task: "Deploy the firmware."
    follow_ups:
      - task: "Report what you deployed."
"#;

/// A run suspended in `deploy`'s follow-up turn, after its work turn
/// completed: the checkpoint it leaves.
async fn interrupted_in_deploy(document: &str) -> (sven_sdk::Harness, sven_sdk::HarnessCheckpoint) {
    let requests = Arc::new(AtomicUsize::new(0));
    let token = CancelToken::new();
    let stop = token.clone();
    let engine = engine(&requests, move |n| {
        if n == 2 {
            stop.cancel();
        }
    });
    let harness = engine.load_harness_from_str(document).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let _ = tokio::time::timeout(
        Duration::from_secs(30),
        run.execute(RunOptions::new().suspend_on(token)),
    )
    .await
    .expect("the run stops");
    let checkpoint = run
        .checkpoint()
        .expect("a suspended run leaves a checkpoint");
    (harness, checkpoint)
}

#[tokio::test]
async fn a_retry_never_task_that_acted_is_not_resumed_into() {
    let (harness, checkpoint) = interrupted_in_deploy(DEPLOY).await;
    let refused = harness.resume(checkpoint).map(|_| ());
    assert!(
        matches!(&refused, Err(CallError::Precondition(why)) if why.contains("retry: never")),
        "{refused:?}"
    );

    // The same interruption of a task without the policy resumes.
    let plain = DEPLOY.replace("    retry: never\n", "");
    let (harness, checkpoint) = interrupted_in_deploy(&plain).await;
    assert!(harness.resume(checkpoint).is_ok());
}
