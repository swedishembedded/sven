// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the lifecycle a service needs - create an agent against a shared
//! engine, advance it one step, suspend it to storage, drop it, and resume it
//! later against that same engine.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in building high-throughput services on top of
//! agent state machines then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{AgentState, Engine};

/// One scripted assistant reply.
fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

/// An engine wired to a scripted provider, with the provider's request log so
/// tests can assert what the model was actually shown.
fn engine_with(scripts: Vec<Vec<ResponseEvent>>) -> (Engine, Arc<ScriptedMockProvider>) {
    let provider = Arc::new(ScriptedMockProvider::new(scripts));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds from a default config");
    (engine, provider)
}

#[tokio::test]
async fn an_agent_answers_a_prompt() {
    let (engine, _p) = engine_with(vec![says("the answer is 42")]);
    let mut agent = engine.agent("agent");

    let reply = agent.send("what is the answer?").await.expect("a reply");

    assert!(
        reply.contains("42"),
        "the agent must return the model's answer, got {reply:?}"
    );
}

#[tokio::test]
async fn a_suspended_agent_resumes_with_the_history_it_had() {
    let (engine, provider) = engine_with(vec![says("blue"), says("I said blue")]);

    let mut agent = engine.agent("agent");
    agent.send("what colour?").await.expect("a first reply");

    // Suspend, round-trip through storage, drop the live object entirely.
    let state = agent.suspend();
    let stored = serde_json::to_string(&state).expect("agent state is serializable");
    drop(state);
    let restored: AgentState = serde_json::from_str(&stored).expect("and deserializable");

    let mut resumed = engine.resume(restored).expect("a resumable state");
    resumed.send("what did you say?").await.expect("a reply");

    let seen = provider
        .last_request
        .lock()
        .unwrap()
        .clone()
        .expect("the provider saw the resumed turn");
    let rendered = format!("{:?}", seen.messages);
    assert!(
        rendered.contains("what colour?") && rendered.contains("blue"),
        "a resumed agent must show the model the conversation it already had; \
         the second turn only carried: {rendered}"
    );
}

#[tokio::test]
async fn suspended_state_survives_without_the_engine_that_made_it() {
    let (engine, _p) = engine_with(vec![says("one")]);
    let mut agent = engine.agent("agent");
    agent.send("hello").await.expect("a reply");
    let stored = serde_json::to_string(&agent.suspend()).expect("serializable");

    // The whole engine goes away - connections, clients, registries, all of it.
    drop(engine);

    let (fresh_engine, _p2) = engine_with(vec![says("two")]);
    let state: AgentState = serde_json::from_str(&stored).expect("deserializable");
    let mut resumed = fresh_engine.resume(state).expect("a resumable state");

    let reply = resumed.send("again").await.expect("a reply");
    assert!(
        reply.contains("two"),
        "state carrying no live handles must resume against any engine, got {reply:?}"
    );
}

#[tokio::test]
async fn two_agents_on_one_engine_keep_separate_histories() {
    let (engine, _p) = engine_with(vec![says("alpha"), says("beta"), says("gamma")]);

    let mut first = engine.agent("agent");
    let mut second = engine.agent("agent");

    first.send("to the first").await.expect("a reply");
    second.send("to the second").await.expect("a reply");

    let first_state = first.suspend();
    let second_state = second.suspend();

    let first_seen = format!("{:?}", first_state.history());
    assert!(
        !first_seen.contains("to the second"),
        "sibling agents on one engine must not share history: {first_seen}"
    );
    assert!(
        format!("{:?}", second_state.history()).contains("to the second"),
        "each agent keeps its own"
    );
}

#[tokio::test]
async fn an_unknown_mode_is_refused_when_the_agent_runs() {
    let (engine, _p) = engine_with(vec![says("never reached")]);
    let mut agent = engine.agent("no-such-mode");

    let err = agent
        .send("hello")
        .await
        .expect_err("an unregistered mode cannot run");

    assert!(
        format!("{err}").contains("no-such-mode"),
        "the failure must name the mode it could not find: {err}"
    );
}

#[tokio::test]
async fn a_turn_whose_model_call_failed_is_an_error_not_an_empty_answer() {
    // A failed turn used to be indistinguishable from a turn that answered
    // with nothing: the executor reports `SessionEvent::Error` and then
    // `TurnComplete`, and `send` returned `Ok("")` for both. Anything that
    // scores or records agent output would have counted the failure as the
    // model declining to answer.
    let provider = Arc::new(sven_model_mock::FailingMockProvider::new(
        "upstream refused the connection",
    ));
    let engine = Engine::builder()
        .model_provider(provider as Arc<_>)
        .build()
        .expect("an engine builds from a default config");
    let mut agent = engine.agent("agent");

    let err = agent
        .send("what is the answer?")
        .await
        .expect_err("a turn whose model call failed must not report success");

    assert!(
        format!("{err:#}").contains("upstream refused the connection"),
        "the failure must carry the reason the turn failed: {err:#}"
    );
}

#[tokio::test]
async fn a_failed_turn_still_leaves_the_agent_resumable() {
    // The error is reported only after the kernel snapshot is stored, so a
    // caller can retry or inspect the agent instead of losing the session.
    let provider = Arc::new(sven_model_mock::FailingMockProvider::new("transport died"));
    let engine = Engine::builder()
        .model_provider(provider as Arc<_>)
        .build()
        .expect("an engine builds from a default config");
    let mut agent = engine.agent("agent");

    let _ = agent.send("hello").await.expect_err("the turn fails");

    let state = agent.suspend();
    let stored = serde_json::to_string(&state).expect("state survives a failed turn");
    let restored: AgentState = serde_json::from_str(&stored).expect("and round-trips");
    assert_eq!(restored.mode(), "agent");
    assert!(
        !restored.history().is_empty(),
        "the user message that provoked the failed turn is still in history"
    );
}

#[tokio::test]
async fn an_application_can_read_what_the_agent_exchanged_without_naming_kernel_types() {
    // The gap this closes: `history()` returns the kernel's message type,
    // which the facade deliberately does not publish, so an application built
    // on the facade alone could obtain the history and had no way to name it.
    // Anything recording a run or deriving training data from it had to reach
    // past the facade, which is what the facade exists to prevent.
    let (engine, _p) = engine_with(vec![says("the answer is 42")]);
    let mut agent = engine.agent("agent");
    agent.send("what is the answer?").await.expect("a reply");

    let transcript = agent.state().transcript();
    assert!(
        transcript.iter().any(
            |t| matches!(t, sven_sdk::Turn::User { text } if text.contains("what is the answer"))
        ),
        "the caller's own message must be in the transcript: {transcript:?}"
    );
    assert!(
        transcript
            .iter()
            .any(|t| matches!(t, sven_sdk::Turn::Assistant { text, .. } if text.contains("42"))),
        "the model's answer must be in the transcript: {transcript:?}"
    );
    // It round-trips, because a run record that cannot be written down is not
    // a run record.
    let json = serde_json::to_string(&transcript).expect("a transcript is serialisable");
    assert!(json.contains("\"turn\""), "{json}");
}
