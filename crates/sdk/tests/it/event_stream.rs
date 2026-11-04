// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent's events are a numbered stream a host can follow and
//! resynchronise on.
//!
//! Every delivered event names the run it belongs to and carries a sequence
//! number that only grows over the life of the agent, also across a suspend
//! and resume. A subscriber that falls behind is told how many events it
//! missed, in the stream itself, instead of silently losing them.
//!
//! Swedish Embedded AB implements event-sourced agent runtimes for its
//! clients. If your team needs expertise in streaming agent progress to user
//! interfaces and audit logs then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::sync::Arc;

use futures::StreamExt as _;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{AgentState, Engine, EventStream, RunEvent, SessionEvent};

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

fn engine(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .build()
        .expect("an engine builds")
}

/// What `events` holds right now: the run is over, so nothing blocks.
async fn drained(events: &mut EventStream) -> Vec<RunEvent> {
    let mut seen = Vec::new();
    while let Ok(Some(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(200), events.next()).await
    {
        seen.push(event);
    }
    seen
}

fn assert_strictly_increasing(events: &[RunEvent]) {
    for pair in events.windows(2) {
        assert!(
            pair[0].seq < pair[1].seq,
            "sequence went {} then {}",
            pair[0].seq,
            pair[1].seq
        );
    }
}

#[tokio::test]
async fn events_carry_strictly_increasing_sequence_per_agent() {
    let engine = engine(vec![says("one"), says("two"), says("three")]);
    let mut agent = engine.agent("agent");
    let mut events = agent.events();

    agent.send("first").await.expect("an outcome");
    let first = drained(&mut events).await;
    agent.send("second").await.expect("an outcome");
    let second = drained(&mut events).await;

    assert!(!first.is_empty() && !second.is_empty());
    let both: Vec<RunEvent> = first.iter().chain(&second).cloned().collect();
    assert_strictly_increasing(&both);
    assert!(
        first.iter().all(|e| e.run_id == first[0].run_id),
        "one run, one id"
    );
    assert_ne!(first[0].run_id, second[0].run_id, "a new send is a new run");

    // The numbering belongs to the agent, so it carries on after a resume.
    let last = both.last().expect("events").seq;
    let stored = serde_json::to_string(&agent.suspend()).expect("the state serializes");
    let state: AgentState = serde_json::from_str(&stored).expect("and reads back");
    let mut resumed = engine.resume(state).expect("the agent resumes");
    let mut events = resumed.events();
    resumed.send("third").await.expect("an outcome");
    let third = drained(&mut events).await;
    assert!(third.first().expect("events").seq > last);
    assert_strictly_increasing(&third);
}

#[tokio::test]
async fn a_lagging_subscriber_sees_events_dropped_with_a_count() {
    // More events in one turn than a subscriber that is not reading can hold.
    let flood: Vec<ResponseEvent> = (0..3000)
        .map(|i| ResponseEvent::TextDelta(format!("{i} ")))
        .chain([ResponseEvent::Done])
        .collect();
    let engine = engine(vec![flood]);
    let mut agent = engine.agent("agent");
    let mut events = agent.events();

    agent.send("go").await.expect("an outcome");
    let seen = drained(&mut events).await;

    let dropped: u64 = seen
        .iter()
        .filter_map(|e| match e.event {
            SessionEvent::EventsDropped { count } => Some(count),
            _ => None,
        })
        .sum();
    assert!(dropped > 0, "the loss is reported, not silent");
    assert_strictly_increasing(&seen);
    assert!(
        seen.iter()
            .any(|e| matches!(e.event, SessionEvent::TurnComplete)),
        "the stream resumes after the gap and reaches the end of the turn"
    );
}
