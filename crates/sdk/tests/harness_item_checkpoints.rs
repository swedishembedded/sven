// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a host that asks for it gets a durable checkpoint each time a
//! `for_each` item completes, so a run killed without warning loses at most
//! the item in flight: resuming the last checkpoint does not run completed
//! items again.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, HarnessCheckpoint, RunOptions};

const SWEEP: &str = r#"
api_version: "2"
name: sweep
inputs: { modules: json }
execution: { limits: { model_turns: 20 } }
workflow:
  - name: each
    for_each: { source: { input: modules }, id: name, max_items: 10 }
    task: "Work on {{ item.name }}."
success:
  work_items: { all_completed: true }
"#;

/// An engine whose model answers every request and counts them.
fn engine(requests: &Arc<AtomicUsize>) -> Engine {
    let counter = Arc::clone(requests);
    Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            vec![ResponseEvent::TextDelta("done".into()), ResponseEvent::Done]
        })) as Arc<_>)
        .build()
        .expect("an engine builds")
}

fn completed_items(checkpoint: &HarnessCheckpoint) -> usize {
    let value = serde_json::to_value(checkpoint).expect("a checkpoint serializes");
    value
        .pointer("/snapshot/context/facts/harness.run/iterations")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|loops| loops.values())
        .flat_map(|state| state["items"].as_array().cloned().unwrap_or_default())
        .filter(|item| item["status"] == "completed")
        .count()
}

#[tokio::test]
async fn each_completed_item_leaves_a_checkpoint_that_resumes_without_repeating_it() {
    let requests = Arc::new(AtomicUsize::new(0));
    let engine = engine(&requests);
    let harness = engine.load_harness_from_str(SWEEP).expect("loads");
    let inputs = json!({"modules": [{"name": "a"}, {"name": "b"}, {"name": "c"}]});

    let checkpoints: Arc<Mutex<Vec<HarnessCheckpoint>>> = Arc::default();
    let sink = Arc::clone(&checkpoints);
    let mut run = harness
        .start(inputs)
        .expect("starts")
        .on_item_checkpoint(move |checkpoint| sink.lock().unwrap().push(checkpoint));
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );
    assert_eq!(requests.load(Ordering::SeqCst), 3, "one turn per item");

    let taken = checkpoints.lock().unwrap().clone();
    assert!(!taken.is_empty(), "completed items left checkpoints");
    let counts: Vec<usize> = taken.iter().map(completed_items).collect();
    assert!(
        counts.windows(2).all(|pair| pair[0] < pair[1]),
        "one checkpoint per newly completed item, in order: {counts:?}"
    );

    // Resume the first of them: its completed items are not run again.
    let first = taken[0].clone();
    let done_before = completed_items(&first);
    assert!(done_before >= 1, "{counts:?}");
    requests.store(0, Ordering::SeqCst);
    let mut resumed = harness.resume(first).expect("the checkpoint fits");
    let outcome = tokio::time::timeout(Duration::from_secs(30), resumed.execute(RunOptions::new()))
        .await
        .expect("the resumed run ends")
        .expect("an outcome");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "{:?}",
        outcome.result
    );
    assert_eq!(
        requests.load(Ordering::SeqCst),
        3 - done_before,
        "only the items that were not completed are worked on"
    );
}
