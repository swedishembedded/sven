// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the history an agent reports is the conversation its model sees.
//!
//! An application stores `AgentState::history()` and resumes from it; the
//! model on the next turn must be given exactly that conversation, tool calls
//! and tool results included, not a reconstruction of it.
//!
//! Swedish Embedded AB implements resumable agent runtimes for its clients.
//! If your team needs expertise in durable agent state then you can procure
//! our services by sending an email to info@swedishembedded.com.

use std::sync::Arc;

use sven_model::{ResponseEvent, Role};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    Engine,
};

struct Lookup;

#[async_trait::async_trait]
impl Tool for Lookup {
    fn name(&self) -> &str {
        "lookup"
    }
    fn description(&self) -> &str {
        "Look a word up."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"word": {"type": "string"}}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "a small rodent")
    }
}

#[tokio::test]
async fn the_next_turn_is_given_exactly_the_reported_history() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "c1".into(),
                name: "lookup".into(),
                arguments: r#"{"word":"vole"}"#.into(),
            },
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::TextDelta("a vole is a rodent".into()),
            ResponseEvent::Done,
        ],
        vec![ResponseEvent::TextDelta("yes".into()), ResponseEvent::Done],
    ]));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .tool(Arc::new(Lookup))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent.send("what is a vole?").await.expect("a first turn");
    let reported = agent.state().history().to_vec();

    let resumed_state = agent.suspend();
    let mut resumed = engine.resume(resumed_state).expect("resumes");
    resumed.send("is it small?").await.expect("a second turn");

    let seen = provider.last_request.lock().unwrap().clone().unwrap();
    let given: Vec<_> = seen
        .messages
        .iter()
        .filter(|m| m.role != Role::System)
        .map(|m| format!("{m:?}"))
        .collect();
    let mut expected: Vec<_> = reported.iter().map(|m| format!("{m:?}")).collect();
    expected.push(format!("{:?}", sven_model::Message::user("is it small?")));
    assert_eq!(given, expected);
    assert!(
        reported.iter().any(|m| m.role == Role::Tool),
        "the tool result is part of the history: {reported:?}"
    );
}
