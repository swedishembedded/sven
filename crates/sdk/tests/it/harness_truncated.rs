// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a reply the provider cuts off at its output-token limit is never a
//! silent success. The turn executor tells the machine, so the task stops
//! with `output_truncated` (and a reply that was whole stops `concluded`).
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, RunOptions};

const DOC: &str = r#"
api_version: "2"
name: cut
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Say a lot.
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
  said: { type: string, value: "{{ tasks.work.output }}" }
"#;

async fn run_with(reply: Vec<ResponseEvent>) -> Value {
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |_| reply.clone())) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(DOC).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");
    match outcome.result {
        Some(Ok(value)) => value,
        other => panic!("expected a completed run: {other:?}"),
    }
}

#[tokio::test]
async fn a_reply_cut_at_the_output_limit_is_reported_as_a_status() {
    let out = run_with(vec![
        ResponseEvent::TextDelta("half of an ans".into()),
        ResponseEvent::MaxTokens,
        ResponseEvent::Done,
    ])
    .await;
    assert_eq!(out["stop"], "output_truncated", "{out}");
    assert_eq!(out["said"], "half of an ans");
}

#[tokio::test]
async fn a_whole_reply_is_concluded() {
    let out = run_with(vec![
        ResponseEvent::TextDelta("a whole answer".into()),
        ResponseEvent::Done,
    ])
    .await;
    assert_eq!(out["stop"], "concluded", "{out}");
}
