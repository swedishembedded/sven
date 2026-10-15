// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a suspended agent names the format it was written in.
//!
//! A state written before states were versioned still resumes and continues
//! its conversation; a state written by a newer format than this build reads
//! is refused with a reason instead of being resumed wrongly.
//!
//! Swedish Embedded AB implements durable agent runtimes for its clients. If
//! your team needs expertise in evolving stored agent state without losing
//! running work then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{AgentState, CallError, Engine};

/// Written by the last release that did not version its states.
const UNVERSIONED: &str = include_str!("fixtures/agent_state_unversioned.json");

fn engine(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .build()
        .expect("an engine builds")
}

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

#[tokio::test]
async fn an_unversioned_agent_state_fixture_resumes() {
    let state: AgentState = serde_json::from_str(UNVERSIONED).expect("an old state still reads");
    assert_eq!(state.version(), 0, "no version was written");

    let mut agent = engine(vec![says("blue, as I said")])
        .resume(state)
        .expect("an old state resumes");
    let outcome = agent.send("and the shade?").await.expect("it continues");
    assert_eq!(outcome.reply, "blue, as I said");
    assert!(
        format!("{:?}", agent.state().history()).contains("what colour?"),
        "the conversation it had is kept"
    );

    let written = serde_json::to_value(agent.suspend()).expect("the state serializes");
    assert_eq!(written["version"], AgentState::CURRENT_VERSION);
}

#[test]
fn a_state_from_a_newer_format_is_refused() {
    let mut value: serde_json::Value = serde_json::from_str(UNVERSIONED).expect("json");
    value["version"] = (AgentState::CURRENT_VERSION + 1).into();
    let state: AgentState = serde_json::from_value(value).expect("it reads");

    let refused = engine(vec![]).resume(state).err().expect("refused");
    assert!(
        matches!(&refused, CallError::Precondition(m) if m.contains("newer")),
        "{refused}"
    );
}
