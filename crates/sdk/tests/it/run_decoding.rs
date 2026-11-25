// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a run's decoding options reach the model request and the trajectory.
//!
//! A training record is only reproducible if the sampling that produced it was
//! asked for on the wire. The options a caller sets on a run (temperature,
//! top-p, top-k, seed, sampled-token capture) must be on every model request
//! of that run, and the sampled tokens and the seed the server used must come
//! back on the trajectory step.
//!
//! Swedish Embedded AB implements agent trajectory capture for its clients.
//! If your team needs expertise in turning agent runs into training data then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_model_mock::MockProvider;
use sven_sdk::model::SamplingParams;
use sven_sdk::{Engine, RunOptions};

fn engine() -> Engine {
    Engine::builder()
        .model_provider(Arc::new(MockProvider))
        .build()
        .expect("an engine builds")
}

#[tokio::test]
async fn a_runs_sampling_and_logprob_request_reach_the_trajectory() {
    let mut agent = engine().agent("agent");
    let sampling = SamplingParams {
        temperature: Some(0.7),
        top_p: Some(0.9),
        top_k: Some(40),
        seed: Some(7),
    };
    agent
        .send_with(
            "hello",
            RunOptions::new().sampling(sampling).want_logprobs(true),
        )
        .await
        .expect("an outcome");

    let trajectory = agent.trajectory();
    let step = trajectory
        .steps
        .iter()
        .find(|s| s.metrics.is_some())
        .expect("the agent step");
    let metrics = step.metrics.as_ref().expect("step metrics");
    assert!(
        metrics
            .completion_token_ids
            .as_ref()
            .is_some_and(|ids| !ids.is_empty()),
        "sampled token ids were requested and recorded"
    );
    assert_eq!(metrics.extra.as_ref().unwrap()["sven"]["seed"], 7);
}

#[tokio::test]
async fn a_run_without_decoding_options_asks_for_no_sampled_tokens() {
    let mut agent = engine().agent("agent");
    agent.send("hello").await.expect("an outcome");
    let trajectory = agent.trajectory();
    let metrics = trajectory
        .steps
        .iter()
        .find(|s| s.metrics.is_some())
        .and_then(|s| s.metrics.as_ref())
        .expect("step metrics");
    assert!(metrics.completion_token_ids.is_none());
}

/// A provider that returns, per generated token, the candidates asked for.
fn scripted(top: bool) -> Engine {
    scripted_with(top, None)
}

/// As [`scripted`], and with `support` the sampler's support the reply carries.
fn scripted_with(top: bool, support: Option<sven_model::SamplerSupport>) -> Engine {
    use sven_model::ResponseEvent;
    let mut events = vec![
        ResponseEvent::PromptTokens { ids: vec![1, 2] },
        ResponseEvent::Tokens {
            ids: vec![11, 12],
            logprobs: vec![-0.5, -0.25],
        },
    ];
    if top {
        events.push(ResponseEvent::TopTokens {
            ids: vec![vec![11, 5, 6], vec![12, 7, 8]],
            logprobs: vec![vec![-0.5, -1.0, -2.0], vec![-0.25, -1.5, -3.0]],
        });
    }
    if let Some(support) = support {
        events.push(ResponseEvent::SamplerSupport(support));
    }
    events.extend([
        ResponseEvent::TextDelta("ok".into()),
        ResponseEvent::Usage {
            input_tokens: 10,
            output_tokens: 2,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
        },
        ResponseEvent::Done,
    ]);
    Engine::builder()
        .model_provider(Arc::new(sven_model_mock::ScriptedMockProvider::new(vec![
            events,
        ])))
        .build()
        .expect("an engine builds")
}

/// Spec: a run that asks for the sampler's top K records each generated
/// position's candidates beside its log-probabilities, on the step.
#[tokio::test]
async fn a_run_that_asks_for_the_top_list_records_it_beside_the_logprobs() {
    let mut agent = scripted(true).agent("agent");
    agent
        .send_with("hello", RunOptions::new().top_logprobs(3))
        .await
        .expect("an outcome");
    let trajectory = agent.trajectory();
    let metrics = trajectory
        .steps
        .iter()
        .find_map(|s| s.metrics.as_ref())
        .expect("step metrics");
    assert_eq!(metrics.logprobs, Some(vec![-0.5, -0.25]));
    let sven = &metrics.extra.as_ref().unwrap()["sven"];
    assert_eq!(
        sven["generation_top_ids"],
        serde_json::json!([[11, 5, 6], [12, 7, 8]])
    );
    assert_eq!(
        sven["generation_top_log_probs"],
        serde_json::json!([[-0.5, -1.0, -2.0], [-0.25, -1.5, -3.0]])
    );
}

/// Spec: a server whose reply lacks the top list it was asked for fails the
/// run by name; the trajectory never holds an empty field in its place.
#[tokio::test]
async fn a_reply_without_the_top_list_it_was_asked_for_is_an_error() {
    let mut agent = scripted(false).agent("agent");
    let outcome = agent
        .send_with("hello", RunOptions::new().top_logprobs(3))
        .await;
    let why = format!("{outcome:?}");
    assert!(
        why.contains("top 3 log-probabilities"),
        "the failure names what is missing: {why}"
    );
    assert!(agent
        .trajectory()
        .steps
        .iter()
        .filter_map(|s| s
            .metrics
            .as_ref()?
            .extra
            .as_ref()?
            .get("sven")?
            .get("generation_top_ids"))
        .next()
        .is_none());
}

/// Spec: a provider of the application's own reports who decoded a reply
/// with the SDK's types alone, and the step records it.
#[tokio::test]
async fn a_providers_reported_identity_reaches_the_step() {
    use sven_sdk::model::{LogprobKind, ResponseEvent, ServedIdentity};
    let identity = ServedIdentity {
        policy_version: Some("3".into()),
        tokenizer_digest: Some("sha256:ab".into()),
        logprob_kind: Some(LogprobKind::Policy),
        logprob_temperature: Some(0.8),
        ..ServedIdentity::default()
    };
    let events = vec![
        ResponseEvent::PromptTokens { ids: vec![1, 2] },
        ResponseEvent::Tokens {
            ids: vec![11],
            logprobs: vec![-0.5],
        },
        ResponseEvent::Identity(identity),
        ResponseEvent::TextDelta("ok".into()),
        ResponseEvent::Done,
    ];
    let engine = Engine::builder()
        .model_provider(Arc::new(sven_model_mock::ScriptedMockProvider::new(vec![
            events,
        ])))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent
        .send_with("hello", RunOptions::new().want_logprobs(true))
        .await
        .expect("an outcome");
    let trajectory = agent.trajectory();
    let sven = trajectory
        .steps
        .iter()
        .find_map(|s| s.metrics.as_ref())
        .and_then(|m| m.extra.as_ref())
        .map(|e| e["sven"].clone())
        .expect("the step's sven figures");
    assert_eq!(sven["policy_version"], "3");
    assert_eq!(sven["logprob_kind"], "policy");
    assert_eq!(sven["logprob_temperature"], 0.8);
    assert_eq!(sven["tokenizer_digest"], "sha256:ab");
}

fn support(cap: u32, offsets: Vec<u32>, ids: Vec<i64>) -> sven_model::SamplerSupport {
    sven_model::SamplerSupport {
        cap,
        log_probs: vec![-0.25, -0.125],
        offsets,
        ids,
    }
}

/// Spec: a run that asks for the sampler's support records it per request beside the log-probabilities.
#[tokio::test]
async fn a_run_that_asks_for_the_support_records_it() {
    let reply = support(4, vec![0, 2, 5], vec![11, 5, 12, 7, 8]);
    let mut agent = scripted_with(false, Some(reply)).agent("agent");
    agent
        .send_with("hello", RunOptions::new().sampler_support(4))
        .await
        .expect("an outcome");
    let trajectory = agent.trajectory();
    let metrics = trajectory
        .steps
        .iter()
        .find_map(|s| s.metrics.as_ref())
        .expect("step metrics");
    assert_eq!(
        metrics.logprobs,
        Some(vec![-0.5, -0.25]),
        "implies logprobs"
    );
    let sven = &metrics.extra.as_ref().unwrap()["sven"];
    assert_eq!(sven["support_cap"], 4);
    assert_eq!(
        sven["generation_sampler_log_probs"],
        serde_json::json!([-0.25, -0.125])
    );
    assert_eq!(
        sven["generation_support_offsets"],
        serde_json::json!([0, 2, 5])
    );
    assert_eq!(
        sven["generation_support_ids"],
        serde_json::json!([11, 5, 12, 7, 8])
    );
}

/// Spec: a reply that lacks the support it was asked for, or whose support does not hold the generated token, fails
/// the run by name; the trajectory never holds a short field in its place.
#[tokio::test]
async fn a_reply_without_a_valid_support_it_was_asked_for_is_an_error() {
    let wrong_token = support(4, vec![0, 2, 5], vec![11, 5, 99, 7, 8]);
    let wrong_cap = support(8, vec![0, 2, 5], vec![11, 5, 12, 7, 8]);
    for reply in [None, Some(wrong_token), Some(wrong_cap)] {
        let mut agent = scripted_with(false, reply).agent("agent");
        let outcome = agent
            .send_with("hello", RunOptions::new().sampler_support(4))
            .await;
        let why = format!("{outcome:?}");
        assert!(
            why.contains("support"),
            "the failure names the field: {why}"
        );
        assert!(agent
            .trajectory()
            .steps
            .iter()
            .filter_map(|s| s
                .metrics
                .as_ref()?
                .extra
                .as_ref()?
                .get("sven")?
                .get("generation_support_ids"))
            .next()
            .is_none());
    }
}
