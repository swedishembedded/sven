// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Doubles shared by the specs of what an embedding host sees of a run: a
//! tool that needs a person's approval, and scripts that call it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::{Agent, Engine, EngineBuilder};

/// A tool that runs commands, so manual approval asks before it runs. Counts
/// how often it really ran.
pub struct Deploy(pub Arc<AtomicUsize>);

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
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "deployed")
    }
}

/// The model calls `deploy` as call `id`.
pub fn deploys(id: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: "deploy".into(),
            arguments: r#"{"env":"prod"}"#.into(),
        },
        ResponseEvent::Done,
    ]
}

/// The model answers with `text`.
pub fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

/// An engine builder serving `scripts`, with the `deploy` tool registered.
pub fn builder(scripts: Vec<Vec<ResponseEvent>>, ran: &Arc<AtomicUsize>) -> EngineBuilder {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .tool(Arc::new(Deploy(Arc::clone(ran))))
}

/// The result the agent's history holds for call `call_id`.
pub fn result_of(agent: &Agent, call_id: &str) -> String {
    agent
        .state()
        .history()
        .iter()
        .find_map(|m| match &m.content {
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } if tool_call_id == call_id => Some(format!("{content:?}")),
            _ => None,
        })
        .unwrap_or_else(|| panic!("call {call_id} has no result"))
}
