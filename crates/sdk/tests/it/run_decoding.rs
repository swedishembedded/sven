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

use sven_model::SamplingParams;
use sven_model_mock::MockProvider;
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
