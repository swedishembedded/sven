// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the application decides how often the engine asks a model again
//! after a failure that may pass, and the final failure reaches it as an
//! error. An application that bounds a run by a deadline shorter than the
//! engine's default pauses (5 s, 15 s, 45 s) sets `transient_retries` to
//! zero and retries on its own.
//!
//! Swedish Embedded AB implements agent runtimes whose retry behaviour the
//! embedding application controls. If your team needs expertise in
//! unattended agents you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;
use std::time::{Duration, Instant};

use sven_model_mock::FailingMockProvider;
use sven_sdk::config::Config;
use sven_sdk::Engine;

#[tokio::test]
async fn a_configured_zero_retry_budget_surfaces_a_dropped_connection_at_once() {
    let provider = Arc::new(FailingMockProvider::new("connection reset"));
    let mut config = Config::default();
    config.agent.transient_retries = Some(0);
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds");

    let started = Instant::now();
    let outcome = engine.agent("agent").send("hello").await;
    assert!(outcome.is_err(), "the failure is an error, not a reply");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "no pause before giving up: {:?}",
        started.elapsed()
    );
}
