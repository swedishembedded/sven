// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for hung and silently stalled model
// connections for its clients. If your team needs expertise in resilient
// agent runtimes then you can procure our services by sending an email to
// info@swedishembedded.com.
//! A streamed model turn must not hang on a server that goes quiet, must keep
//! a live but slow model going, and must say what it is waiting for.

use std::time::Duration;

use sven_model_mock::{StallMode, StallingMockProvider};
use sven_turn::{stream_turn, StallLimits, TurnLimits};
use sven_vocab::{ModelWaitPhase, SessionEvent};
use tokio::sync::mpsc;
use tokio::time::Instant;

const KEEPALIVE_EVERY: Duration = Duration::from_secs(10);

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn limits(first_byte: u64, first_token: u64, no_progress: u64) -> TurnLimits {
    TurnLimits::default().with_stall_limits(StallLimits::from_secs(
        Some(first_byte),
        Some(first_token),
        Some(no_progress),
    ))
}

/// Runs one turn to its end, returning the outcome and every event it sent.
async fn run(
    provider: &StallingMockProvider,
    limits: TurnLimits,
) -> (anyhow::Result<sven_turn::StreamedTurn>, Vec<SessionEvent>) {
    let (tx, mut rx) = mpsc::channel(256);
    let result = stream_turn(
        provider,
        vec![],
        vec![],
        None,
        None,
        None,
        None,
        limits,
        &tx,
    )
    .await;
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    (result, events)
}

fn waiting_phases(events: &[SessionEvent]) -> Vec<ModelWaitPhase> {
    events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ModelWaiting { phase, .. } => Some(*phase),
            _ => None,
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_server_that_never_sends_headers_fails_at_the_first_byte_limit() {
    let provider = StallingMockProvider::new(StallMode::NeverReturns).with_host("llm.example:8788");
    let started = Instant::now();
    let (result, _) = run(&provider, limits(30, 60, 90)).await;
    let err = result.expect_err("a server that never answers must fail the turn");
    assert!(
        err.to_string()
            .contains("no response headers from stalling-mock llm.example:8788 after 30 s"),
        "{err}"
    );
    assert_eq!(started.elapsed(), secs(30));
}

#[tokio::test(start_paused = true)]
async fn a_server_that_sends_headers_and_nothing_fails_at_the_first_token_limit() {
    let provider = StallingMockProvider::new(StallMode::HeadersThenSilent);
    let started = Instant::now();
    let (result, _) = run(&provider, limits(30, 60, 90)).await;
    let err = result.expect_err("silence must fail the turn");
    assert!(err.to_string().contains("no model output"), "{err}");
    assert!(err.to_string().contains("after 60 s"), "{err}");
    assert_eq!(started.elapsed(), secs(60));
}

#[tokio::test(start_paused = true)]
async fn keepalives_alone_do_not_satisfy_the_first_token_limit() {
    let provider = StallingMockProvider::new(StallMode::KeepaliveOnly {
        interval: KEEPALIVE_EVERY,
    });
    let started = Instant::now();
    let (result, _) = run(&provider, limits(30, 60, 90)).await;
    let err = result.expect_err("keepalives are not output");
    assert!(err.to_string().contains("only keepalives"), "{err}");
    assert!(err.to_string().contains("after 60 s"), "{err}");
    assert_eq!(started.elapsed(), secs(60));
}

#[tokio::test(start_paused = true)]
async fn keepalives_after_the_first_token_fail_at_the_no_progress_limit() {
    let provider = StallingMockProvider::new(StallMode::KeepaliveOnly {
        interval: KEEPALIVE_EVERY,
    })
    .after_real_deltas(1);
    let started = Instant::now();
    let (result, _) = run(&provider, limits(30, 60, 90)).await;
    let err = result.expect_err("a model that stopped producing must fail the turn");
    assert!(err.to_string().contains("only keepalives"), "{err}");
    assert!(err.to_string().contains("for 90 s"), "{err}");
    assert_eq!(started.elapsed(), secs(90));
}

#[tokio::test(start_paused = true)]
async fn a_slow_model_that_keeps_producing_is_not_aborted() {
    // 40 chunks, one every 20 s: 800 s in all, far past every limit, but no
    // gap between two chunks reaches the 90 s no-progress limit.
    let provider = StallingMockProvider::new(StallMode::Progressing {
        interval: secs(20),
        deltas: 40,
    });
    let (result, events) = run(&provider, limits(30, 60, 90)).await;
    let turn = result.expect("a live model must finish its turn");
    assert_eq!(turn.text, "tok ".repeat(40));
    assert!(
        !waiting_phases(&events).contains(&ModelWaitPhase::Connecting),
        "{events:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn waiting_is_reported_every_few_seconds_with_its_phase() {
    let provider = StallingMockProvider::new(StallMode::NeverReturns);
    let (_, events) = run(&provider, limits(30, 60, 90)).await;
    // Notices at 5, 10, ..., 25 s; the turn fails at 30 s.
    assert_eq!(
        waiting_phases(&events),
        vec![ModelWaitPhase::Connecting; 5],
        "{events:?}"
    );
    assert!(matches!(
        events[1],
        SessionEvent::ModelWaiting {
            elapsed_secs: 10,
            ..
        }
    ));

    let provider = StallingMockProvider::new(StallMode::KeepaliveOnly {
        interval: KEEPALIVE_EVERY,
    })
    .after_real_deltas(1);
    let (_, events) = run(&provider, limits(30, 60, 90)).await;
    let phases = waiting_phases(&events);
    assert!(!phases.is_empty());
    assert!(
        phases.iter().all(|p| *p == ModelWaitPhase::Idle),
        "{phases:?}"
    );

    let provider = StallingMockProvider::new(StallMode::HeadersThenSilent);
    let (_, events) = run(&provider, limits(30, 60, 90)).await;
    assert!(waiting_phases(&events)
        .iter()
        .all(|p| *p == ModelWaitPhase::AwaitingFirstToken));
}

#[tokio::test(start_paused = true)]
async fn a_user_cancel_aborts_a_waiting_turn_promptly() {
    let provider = StallingMockProvider::new(StallMode::NeverReturns);
    let (tx, mut rx) = mpsc::channel(256);
    let (cancel, cancelled) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        tokio::time::sleep(secs(7)).await;
        let _ = cancel.send(());
    });
    let started = Instant::now();
    let turn = stream_turn(
        &provider,
        vec![],
        vec![],
        None,
        None,
        None,
        None,
        limits(600, 600, 600),
        &tx,
    );
    tokio::select! {
        _ = cancelled => {}
        _ = turn => panic!("the turn cannot finish before the cancel"),
    }
    assert_eq!(started.elapsed(), secs(7));
    drop(tx);
    assert!(matches!(
        rx.recv().await,
        Some(SessionEvent::ModelWaiting {
            elapsed_secs: 5,
            ..
        })
    ));
}
