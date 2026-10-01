// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every tool call in the conversation has a result, even one that
//! never ran.
//!
//! Providers reject a history in which an assistant tool call is not
//! followed by its result. A call refused before it runs - a person said no,
//! the mode forbids it - must still be answered in the thread, with the
//! reason, so the next turn is a valid request and the model learns why.
//!
//! Swedish Embedded AB implements robust agent runtimes for its clients. If
//! your team needs expertise in provider-compatible agent state then you can
//! procure our services by sending an email to info@swedishembedded.com.

mod common;

use std::sync::Arc;

use common::Strict;
use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::ToolCapability,
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    ApprovalPolicy, Engine, HumanGate,
};

struct Deploy(sven_sdk::tool::ToolCapability);

#[async_trait::async_trait]
impl Tool for Deploy {
    fn name(&self) -> &str {
        "deploy"
    }
    fn description(&self) -> &str {
        "Deploy to production."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> sven_sdk::tool::ToolCapability {
        self.0
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "deployed")
    }
}

/// Runs a turn whose one `deploy` call the engine refuses, and returns the
/// result the call got in the thread.
async fn refused_result(engine: sven_sdk::EngineBuilder) -> String {
    let scripts = vec![
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "d1".into(),
                name: "deploy".into(),
                arguments: "{}".into(),
            },
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::TextDelta("not deployed".into()),
            ResponseEvent::Done,
        ],
    ];
    let engine = engine
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = agent
        .send("ship it")
        .await
        .expect("the turn after the refusal is a valid request");
    assert_eq!(outcome.reply, "not deployed");
    agent
        .state()
        .history()
        .iter()
        .find_map(|m| match &m.content {
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } if tool_call_id == "d1" => Some(format!("{content:?}")),
            _ => None,
        })
        .expect("the refused call has a result")
}

#[tokio::test]
async fn a_call_the_mode_forbids_is_answered_in_the_thread() {
    // The agent mode does not allow driving a device.
    let result =
        refused_result(Engine::builder().tool(Arc::new(Deploy(ToolCapability::ControlDevice))))
            .await;
    assert!(result.contains("not run"), "the result says why: {result}");
}

#[tokio::test]
async fn a_call_a_person_refused_is_answered_in_the_thread() {
    let result = refused_result(
        Engine::builder()
            .tool(Arc::new(Deploy(ToolCapability::ExecuteShell)))
            .approvals(ApprovalPolicy::Manual)
            .human_gates(|gate| {
                if let HumanGate::Approval { reply_tx, .. } = gate {
                    let _ = reply_tx.send(false);
                }
            }),
    )
    .await;
    assert!(result.contains("not run"), "the result says why: {result}");
}
