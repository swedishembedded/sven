// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reproducible agent trajectories
// for its clients. If your team needs expertise in training-grade run records
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The echo mock honours the sampling seam deterministically, so tests of
//! trajectory recording need no real model.

use futures::StreamExt;
use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent, SamplingParams};
use sven_model_mock::MockProvider;

async fn run(req: CompletionRequest) -> Vec<ResponseEvent> {
    let mut s = MockProvider.complete(req).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = s.next().await {
        out.push(ev.unwrap());
    }
    out
}

#[tokio::test]
async fn logprobs_requested_yields_one_id_and_logprob_per_reply_byte() {
    let req = CompletionRequest {
        messages: vec![Message::user("ab")],
        sampling: SamplingParams {
            seed: Some(3),
            ..Default::default()
        },
        want_logprobs: true,
        ..Default::default()
    };
    let a = run(req.clone()).await;
    let b = run(req).await;
    let tokens = a
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Tokens { ids, logprobs } => Some((ids.clone(), logprobs.clone())),
            _ => None,
        })
        .expect("a Tokens event");
    assert_eq!(tokens.0.len(), "MOCK: ab".len());
    assert_eq!(tokens.0.len(), tokens.1.len());
    assert!(tokens.1.iter().all(|lp| *lp < 0.0));
    assert!(a.iter().any(|e| matches!(
        e,
        ResponseEvent::Served { model, seed } if model == "mock-model" && *seed == Some(3)
    )));
    assert_eq!(format!("{a:?}"), format!("{b:?}"), "deterministic");
}

#[tokio::test]
async fn without_the_flag_no_token_events_are_emitted() {
    let events = run(CompletionRequest {
        messages: vec![Message::user("ab")],
        ..Default::default()
    })
    .await;
    assert!(!events.iter().any(|e| matches!(
        e,
        ResponseEvent::Tokens { .. } | ResponseEvent::Served { .. }
    )));
}
