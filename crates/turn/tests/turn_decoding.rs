// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements reproducible agent runs on locally served
// models for its clients. If your team needs expertise in recording exactly
// how an agent's model was sampled then you can procure our services by
// sending an email to info@swedishembedded.com.
//! Spec: one turn's own decoding settings lie over the run's, reach the
//! provider in the request, and are recorded with the request's account.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream, SamplingParams};
use sven_turn::{stream_turn, TurnLimits};
use sven_vocab::{SessionEvent, TurnDecoding};
use tokio::sync::mpsc;

/// What a request was sent with: its sampling and its thinking switch.
type Sent = (SamplingParams, Option<bool>);

/// A provider that answers "ok" and keeps the requests it was sent.
#[derive(Default)]
struct Capturing {
    seen: Arc<Mutex<Vec<Sent>>>,
}

#[async_trait]
impl ModelProvider for Capturing {
    fn name(&self) -> &str {
        "capturing"
    }
    fn model_name(&self) -> &str {
        "m"
    }
    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        self.seen
            .lock()
            .unwrap()
            .push((req.sampling, req.enable_thinking));
        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta("ok".into())),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

async fn run(limits: TurnLimits) -> (Vec<Sent>, Vec<SessionEvent>) {
    let provider = Capturing::default();
    let (tx, mut rx) = mpsc::channel(64);
    stream_turn(
        &provider,
        vec![],
        vec![],
        None,
        None,
        None,
        None,
        limits,
        &tx,
    )
    .await
    .expect("the turn completes");
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let seen = provider.seen.lock().unwrap().clone();
    (seen, events)
}

fn step_settings() -> TurnDecoding {
    TurnDecoding {
        temperature: Some(0.2),
        top_k: Some(40),
        thinking: Some(false),
        thinking_budget: Some(256),
        ..TurnDecoding::default()
    }
}

#[tokio::test]
async fn a_turns_settings_replace_the_runs_field_by_field_and_reach_the_provider() {
    let run_wide = TurnLimits::default().with_decoding(sven_turn::Decoding {
        sampling: SamplingParams {
            temperature: Some(0.9),
            top_p: Some(0.8),
            ..SamplingParams::default()
        },
        want_logprobs: false,
    });
    let (seen, _) = run(run_wide.with_turn_decoding(&step_settings())).await;
    let (sampling, thinking) = seen[0];
    assert_eq!(sampling.temperature, Some(0.2), "the turn's value wins");
    assert_eq!(sampling.top_p, Some(0.8), "an unset one stays the run's");
    assert_eq!(sampling.top_k, Some(40));
    assert_eq!(thinking, Some(false));
}

#[tokio::test]
async fn the_settings_a_request_used_are_recorded_with_it() {
    let limits = TurnLimits::default().with_turn_decoding(&step_settings());
    let (_, events) = run(limits).await;
    let sampled = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::ResponseSampled(s) => Some(s.clone()),
            _ => None,
        })
        .expect("a request with settings reports them");
    assert_eq!(sampled.decoding.as_deref(), Some(&step_settings()));
}

#[tokio::test]
async fn a_turn_without_settings_adds_nothing_to_the_record() {
    let (seen, events) =
        run(TurnLimits::default().with_turn_decoding(&TurnDecoding::default())).await;
    assert_eq!(seen[0], (SamplingParams::default(), None));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::ResponseSampled(_))),
        "nothing was set, so nothing is reported"
    );
}

#[tokio::test]
async fn a_turn_can_bound_its_response_and_its_wait_for_the_first_one() {
    let limits = TurnLimits::default().with_turn_decoding(&TurnDecoding {
        max_output_tokens: Some(333),
        first_response_timeout: Some(90),
        ..TurnDecoding::default()
    });
    assert_eq!(limits.max_output_tokens, Some(333));
    assert_eq!(
        limits.stall.first_byte,
        Some(std::time::Duration::from_secs(90))
    );
    assert_eq!(
        limits.stall.first_token,
        Some(std::time::Duration::from_secs(90))
    );
    let recorded = limits.recorded_decoding();
    assert_eq!(recorded.max_output_tokens, Some(333));
    assert_eq!(recorded.first_response_timeout, Some(90));
    // The turn without them leaves the run's bounds alone.
    let base = TurnLimits::default().with_max_output_tokens(Some(100));
    assert_eq!(
        base.with_turn_decoding(&TurnDecoding::default())
            .max_output_tokens,
        Some(100)
    );
}
