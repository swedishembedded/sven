// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a host that asks for it is handed the conversations of a `for_each`
//! item when the item finishes, while the run is still going, so a run that is
//! stopped keeps the transcripts of the items it finished.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use serde_json::json;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::{Engine, RunOptions};
use tokio::sync::Notify;

const SWEEP: &str = r#"
api_version: "2"
name: sweep
inputs: { modules: json }
execution: { limits: { model_turns: 20 } }
workflow:
  - name: each
    for_each: { source: { input: modules }, id: name, max_items: 10 }
    task: "Work on {{ item.name }}."
"#;

/// Answers every request; the second one waits until the host has been handed
/// the first item's conversation, and notes if that never happened.
struct WaitsForDelivery {
    requests: AtomicUsize,
    delivered: Arc<Notify>,
    starved: Arc<AtomicBool>,
}

#[async_trait]
impl ModelProvider for WaitsForDelivery {
    fn name(&self) -> &str {
        "waits"
    }
    fn model_name(&self) -> &str {
        "waits-model"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        if self.requests.fetch_add(1, Ordering::SeqCst) == 1
            && tokio::time::timeout(Duration::from_secs(10), self.delivered.notified())
                .await
                .is_err()
        {
            self.starved.store(true, Ordering::SeqCst);
        }
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::ThinkingDelta("thinking".into())),
            Ok(ResponseEvent::TextDelta("done".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

#[tokio::test]
async fn a_finished_items_conversation_is_handed_over_before_the_next_item_ends() {
    let delivered = Arc::new(Notify::new());
    let starved = Arc::new(AtomicBool::new(false));
    let provider = WaitsForDelivery {
        requests: AtomicUsize::new(0),
        delivered: Arc::clone(&delivered),
        starved: Arc::clone(&starved),
    };
    let engine = Engine::builder()
        .model_provider(Arc::new(provider) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(SWEEP).expect("loads");

    let seen: Arc<Mutex<Vec<(usize, String, usize)>>> = Arc::default();
    let (sink, wake) = (Arc::clone(&seen), Arc::clone(&delivered));
    let mut run = harness
        .start(json!({"modules": [{"name": "a"}, {"name": "b"}]}))
        .expect("starts")
        .on_item_conversations(move |updates| {
            let mut seen = sink.lock().unwrap();
            for update in updates {
                seen.push((update.index, update.name, update.trajectory.steps.len()));
            }
            wake.notify_one();
        });
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");

    assert!(
        !starved.load(Ordering::SeqCst),
        "the first item's conversation arrived before the second item's turn finished"
    );
    // An update may carry the next item's conversation while it is under
    // way; the latest update of each conversation is its whole.
    let mut latest = std::collections::BTreeMap::new();
    for (index, name, steps) in seen.lock().unwrap().iter() {
        latest.insert(*index, (name.clone(), *steps));
    }
    assert_eq!(
        latest.into_iter().collect::<Vec<_>>(),
        [
            (0, ("each#0".to_string(), 2)),
            (1, ("each#1".to_string(), 2))
        ]
    );
}
