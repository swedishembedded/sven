// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a model reply with no text and no tool call is not an answer and not
//! a side-effect failure. A turn a harness asked for is asked again, twice at
//! most, each retry on the trace; when the replies stay empty the run fails
//! with `empty_reply`; a prompt too large for the context is named for that.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in running unattended agents on
//! servers that sometimes answer with nothing then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_model::ResponseEvent;
use sven_model_mock::CallbackProvider;
use sven_sdk::{Engine, HarnessOutcome, RunOptions, TraceRecord};

const DOC: &str = r#"
api_version: "2"
name: quiet
execution: { limits: { model_turns: 12 } }
workflow:
  - name: work
    task: Say something.
outputs:
  said: { type: string, value: "{{ tasks.work.output }}" }
"#;

/// A model whose n-th reply (from 0) is `reply(n)`; `None` is an empty one.
async fn run_with(
    reply: impl Fn(usize) -> Option<&'static str> + Send + Sync + 'static,
) -> (HarnessOutcome, usize) {
    let asked = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&asked);
    let engine = Engine::builder()
        .model_provider(Arc::new(CallbackProvider::new(move |_| {
            match reply(counter.fetch_add(1, Ordering::SeqCst)) {
                Some(text) => vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done],
                None => vec![ResponseEvent::Done],
            }
        })) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(DOC).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");
    (outcome, asked.load(Ordering::SeqCst))
}

fn retries(outcome: &HarnessOutcome) -> Vec<u64> {
    outcome
        .trace
        .iter()
        .filter_map(|r| match r {
            TraceRecord::TurnRetried {
                attempt, reason, ..
            } if reason == "empty_reply" => Some(*attempt),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_empty_reply_is_asked_again_and_the_retry_is_on_the_trace() {
    let (outcome, asked) = run_with(|n| (n >= 2).then_some("finally")).await;
    assert!(
        matches!(&outcome.result, Some(Ok(v)) if v["said"] == "finally"),
        "{:?}",
        outcome.result
    );
    assert_eq!(asked, 3);
    assert_eq!(retries(&outcome), [1, 2]);
}

#[tokio::test]
async fn replies_that_stay_empty_end_the_run_as_empty_reply() {
    let (outcome, asked) = run_with(|_| None).await;
    let Some(Err(failure)) = &outcome.result else {
        panic!("expected a failure: {:?}", outcome.result);
    };
    assert_eq!(failure.code, "empty_reply", "{failure:?}");
    assert_eq!(asked, 3, "the first ask and two retries");
    assert_eq!(retries(&outcome), [1, 2]);
}

/// A server that always says the prompt does not fit.
struct TooLarge;

#[async_trait::async_trait]
impl sven_model::ModelProvider for TooLarge {
    fn name(&self) -> &str {
        "too-large"
    }
    fn model_name(&self) -> &str {
        "too-large"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<sven_model::ResponseStream> {
        anyhow::bail!(
            "HTTP 400: context_length_exceeded: the request has 70000 tokens, the context is 65536"
        )
    }
}

#[tokio::test]
async fn a_prompt_that_does_not_fit_the_context_is_named_for_it() {
    let engine = Engine::builder()
        .model_provider(Arc::new(TooLarge) as Arc<_>)
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(DOC).expect("loads");
    let mut run = harness.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(30), run.execute(RunOptions::new()))
        .await
        .expect("ends")
        .expect("an outcome");
    let Some(Err(failure)) = &outcome.result else {
        panic!("expected a failure: {:?}", outcome.result);
    };
    assert_eq!(failure.code, "context_exhausted", "{failure:?}");
}
