// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a turn the thinking watchdog cuts short is not a user cancel. In a
//! harness run it is the stop status `watchdog`, which does not fail the run
//! by itself (the task's checks decide), and a run that did fail never
//! reports the conclusion `Success`.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in running unattended agents
//! that report why they stopped then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, HarnessOutcome, RunConclusion, RunOptions};

const DOC: &str = r#"
api_version: "2"
name: looping
execution: { limits: { model_turns: 4 } }
workflow:
  - name: work
    task: Think about it.
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
"#;

async fn run_with(reply: Vec<ResponseEvent>) -> HarnessOutcome {
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |_| reply.clone())) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(DOC).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome")
}

#[tokio::test]
async fn a_reasoning_loop_is_a_stop_status_and_not_a_cancel() {
    let sentence = "Let me reconsider the whole problem from the start once more, carefully. ";
    let mut reply: Vec<ResponseEvent> = (0..80)
        .map(|_| ResponseEvent::ThinkingDelta(sentence.into()))
        .collect();
    reply.push(ResponseEvent::Done);
    let outcome = run_with(reply).await;
    match &outcome.result {
        Some(Ok(out)) => assert_eq!(out["stop"], "watchdog", "{out}"),
        other => panic!("a watchdog abort must not fail the run: {other:?}"),
    }
    assert_eq!(outcome.conclusion, RunConclusion::Success);
}

#[tokio::test]
async fn a_failed_run_never_concludes_as_success() {
    let outcome = run_with(vec![ResponseEvent::Done]).await;
    let Some(Err(failure)) = &outcome.result else {
        panic!("empty replies fail the run: {:?}", outcome.result);
    };
    assert_eq!(failure.code, "empty_reply");
    assert_eq!(outcome.conclusion, RunConclusion::AgentError);
}
