// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.

//! Contract: a brain server's answer is recorded the same whichever transport
//! carried it. One reply (the sampled and top log-probabilities, the identity of
//! the decode, the sampler's support, a cut-off) is served over the HTTP chat API and over the D-Bus
//! `Run` result, and the per-request record folded from each provider's events
//! is compared field by field, so a trainer reading a trajectory cannot tell the
//! transports apart and a field one provider drops is a failing test.

#![cfg(all(target_os = "linux", feature = "dbus"))]

use futures::StreamExt;
use serde_json::{json, Value};
use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent};
use sven_model_drivers::{from_config, ModelConfig};
use sven_vocab::{Sampled, ServedIdentity};

use crate::dbus_provider_test::fake_server_replying;
use crate::sampling_wire::{mock_server_once, sse_body};

/// What a brain server says of one decode, once: the same keys on the `brain`
/// object of an HTTP frame and at the top of a D-Bus `Run` result.
fn decode() -> Value {
    json!({
        "seed": 7,
        "adapter": {"id": "lab", "digest": "sha256:aa", "version": 4, "lineage": {"run": "k", "epoch": 2, "update": 9, "base_digest": "sha256:bb", "adapter_digest": "sha256:aa"}},
        "base_digest": "sha256:bb",
        "backend": "cuda",
        "engine": "brain 6.0.0",
        "tokenizer_digest": "sha256:tt",
        "chat_template_digest": "sha256:cc",
        "logprob_kind": "policy",
    })
}

/// The policy trace of the same reply: two generated tokens, their top-3.
fn trace() -> Value {
    json!({
        "prompt_token_ids": [1, 2, 3],
        "generation_token_ids": [21, 22],
        "generation_log_probs": [-0.5, -0.75],
        "log_prob_temperature": 0.8,
        "top_log_probs": 3,
        "generation_top_ids": [[21, 5, 6], [22, 7, 8]],
        "generation_top_log_probs": [[-0.5, -1.5, -2.5], [-0.75, -1.25, -3.0]],
        "support_cap": 8,
        "generation_sampler_log_probs": [-0.25, -0.5],
        "generation_support_offsets": [0, 2, 5],
        "generation_support_ids": [21, 9, 22, 7, 8],
    })
}

fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message::user("hello")],
        stream: true,
        want_logprobs: true,
        top_logprobs: Some(3),
        sampler_support: Some(8),
        ..Default::default()
    }
}

/// The record the turn folds from a response's events: every fact the
/// trajectory keeps of one request, whichever provider produced the events.
fn folded(events: &[ResponseEvent]) -> (Sampled, bool) {
    let mut sampled = Sampled::default();
    let mut identity = ServedIdentity::default();
    let mut cut_off = false;
    for event in events {
        match event {
            ResponseEvent::Tokens { ids, logprobs } => {
                sampled.token_ids.extend(ids);
                sampled.logprobs.extend(logprobs);
            }
            ResponseEvent::TopTokens { ids, logprobs } => {
                sampled.top_token_ids.extend(ids.clone());
                sampled.top_logprobs.extend(logprobs.clone());
            }
            ResponseEvent::SamplerSupport(support) => {
                sampled.sampler_support = Some(support.clone())
            }
            ResponseEvent::PromptTokens { ids } => sampled.prompt_token_ids.extend(ids),
            ResponseEvent::Identity(i) => identity.merge(i.clone()),
            ResponseEvent::MaxTokens => cut_off = true,
            _ => {}
        }
    }
    sampled.identity = identity;
    (sampled, cut_off)
}

async fn over_http() -> (Sampled, bool, Value) {
    let frame = json!({
        "model": "m",
        "brain": decode(),
        "policy_trace": trace(),
        "choices": [{"delta": {}, "finish_reason": "length"}],
    })
    .to_string();
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse_body(&[&frame])).await;
    let config = ModelConfig {
        provider: "brain".into(),
        name: "m".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let mut stream = from_config(&config)
        .unwrap()
        .complete(request())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("a stream event"));
    }
    let (sampled, cut_off) = folded(&events);
    (
        sampled,
        cut_off,
        req_rx.await.unwrap().body["policy_trace"].clone(),
    )
}

async fn over_dbus() -> (Sampled, bool, Value) {
    let mut result = decode();
    result["policy_trace"] = trace();
    result["finish_reason"] = json!("length");
    result["usage"] = json!({"input_tokens": 3, "output_tokens": 2});
    let (provider, seen, _server) = fake_server_replying(Some(result)).await;
    let mut stream = provider.complete(request()).await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("a stream event"));
    }
    let (sampled, cut_off) = folded(&events);
    let params: Value =
        serde_json::from_str(&seen.lock().unwrap().clone().unwrap().params).unwrap();
    (
        sampled,
        cut_off,
        json!([
            params["policy_trace"],
            params["policy_trace_top_log_probs"],
            params["policy_trace_support"]
        ]),
    )
}

#[tokio::test]
async fn http_and_dbus_record_one_reply_as_the_same_request() {
    let (http, http_cut, http_asked) = over_http().await;
    let (dbus, dbus_cut, dbus_asked) = over_dbus().await;

    // What each transport was asked for: the same quantity, in its own wire shape.
    assert_eq!(http_asked, json!({"top_log_probs": 3, "support": 8}));
    assert_eq!(dbus_asked, json!([true, 3, 8]));

    // Field by field, so a failure names the field.
    assert_eq!(http.prompt_token_ids, dbus.prompt_token_ids, "prompt ids");
    assert_eq!(http.token_ids, dbus.token_ids, "sampled ids");
    assert_eq!(http.logprobs, dbus.logprobs, "log-probabilities");
    assert_eq!(http.top_token_ids, dbus.top_token_ids, "top ids");
    assert_eq!(
        http.top_logprobs, dbus.top_logprobs,
        "top log-probabilities"
    );
    assert_eq!(
        http.sampler_support, dbus.sampler_support,
        "sampler support"
    );
    assert_eq!(http_cut, dbus_cut, "cut off at the token ceiling");
    let (h, d) = (&http.identity, &dbus.identity);
    assert_eq!(h.seed, d.seed, "seed");
    assert_eq!(h.adapter, d.adapter, "adapter");
    assert_eq!(h.base_digest, d.base_digest, "base digest");
    assert_eq!(h.backend, d.backend, "backend");
    assert_eq!(h.engine, d.engine, "engine");
    assert_eq!(h.policy_version, d.policy_version, "policy version");
    assert_eq!(h.policy_lineage, d.policy_lineage, "policy lineage");
    assert_eq!(h.tokenizer_digest, d.tokenizer_digest, "tokenizer digest");
    assert_eq!(
        h.chat_template_digest, d.chat_template_digest,
        "chat template digest"
    );
    assert_eq!(h.logprob_kind, d.logprob_kind, "log-probability kind");
    assert_eq!(
        h.logprob_temperature, d.logprob_temperature,
        "log-probability temperature"
    );

    // And the record is the whole reply, not an empty agreement.
    assert_eq!(http.token_ids, vec![21, 22]);
    assert_eq!(http.top_token_ids, vec![vec![21, 5, 6], vec![22, 7, 8]]);
    let support = http.sampler_support.as_ref().expect("the support");
    assert_eq!((support.cap, support.ids.len()), (8, 5));
    assert!(http_cut);
    assert_eq!(h.policy_version.as_deref(), Some("4"));
    assert_eq!(
        h.policy_lineage
            .as_ref()
            .map(|l| (l.run.as_str(), l.epoch, l.update)),
        Some(("k", 2, 9))
    );
    assert_eq!(h.logprob_temperature, Some(0.8));
}
