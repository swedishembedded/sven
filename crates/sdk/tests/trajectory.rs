// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent's work exports as a valid ATIF trajectory.
//!
//! The trajectory is what the agent did, in the interchange format training
//! and evaluation tools read: every message, every tool call with its
//! observed result, the model it ran on and the tools it was offered. It is
//! built from the conversation the kernel holds, so it survives a
//! suspend/resume round trip unchanged.
//!
//! Swedish Embedded AB implements agent trajectory capture for its clients.
//! If your team needs expertise in turning agent runs into training data then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

mod common;

use std::sync::Arc;

use common::Strict;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    AgentState, Engine,
};

struct Weather;

#[async_trait::async_trait]
impl Tool for Weather {
    fn name(&self) -> &str {
        "weather"
    }
    fn kernel_capability(&self) -> sven_sdk::tool::ToolCapability {
        sven_sdk::tool::ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Today's weather in a city."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "sunny")
    }
}

#[tokio::test]
async fn a_run_exports_as_a_valid_trajectory() {
    let scripts = vec![
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "w1".into(),
                name: "weather".into(),
                arguments: r#"{"city":"Lund"}"#.into(),
            },
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::TextDelta("It is sunny in Lund.".into()),
            ResponseEvent::Done,
        ],
    ];
    let engine = Engine::builder()
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .tool(Arc::new(Weather))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent.send("weather in Lund?").await.expect("an outcome");

    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");
    assert_eq!(trajectory.agent.model_name.as_deref(), Some("strict"));
    let offered = serde_json::to_string(&trajectory.agent.tool_definitions).unwrap();
    assert!(offered.contains("\"weather\""), "{offered}");

    let json = serde_json::to_value(&trajectory).unwrap();
    let steps = json["steps"].as_array().expect("steps");
    let text = serde_json::to_string(steps).unwrap();
    for needle in [
        "weather in Lund?",
        "\"Lund\"",
        "sunny",
        "It is sunny in Lund.",
    ] {
        assert!(text.contains(needle), "{needle} missing from {text}");
    }

    // Built from the durable state, so a resumed agent exports the same.
    let state: AgentState =
        serde_json::from_str(&serde_json::to_string(agent.state()).unwrap()).unwrap();
    let resumed = engine.resume(state).expect("resumes");
    assert_eq!(
        serde_json::to_value(resumed.trajectory()).unwrap(),
        json,
        "the trajectory survives suspend/resume"
    );
}
