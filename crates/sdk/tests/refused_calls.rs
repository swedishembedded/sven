// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every tool call in the conversation has a result, even one that
//! never ran.
//!
//! Providers reject a history in which an assistant tool call is not
//! followed by its result. A call refused before it runs - a human said no,
//! the policy forbids it - must still be answered in the thread, with the
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
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    Engine,
};

struct Deploy;

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
        // The agent mode does not allow driving a device.
        sven_sdk::tool::ToolCapability::ControlDevice
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "deployed")
    }
}

#[tokio::test]
async fn a_call_the_policy_refused_is_answered_in_the_thread() {
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
    let engine = Engine::builder()
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .tool(Arc::new(Deploy))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = agent
        .send("ship it")
        .await
        .expect("the turn after the refusal is a valid request");
    assert_eq!(outcome.reply, "not deployed");
    let result = agent
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
        .expect("the refused call has a result");
    assert!(result.contains("not run"), "the result says why: {result}");
}
