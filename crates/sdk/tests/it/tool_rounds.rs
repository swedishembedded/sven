// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent keeps going for as many tool rounds as the model asks for,
//! whatever shape the model's stream takes.
//!
//! A model that sends no usage chunk and splits a call's arguments across
//! several deltas is still a model: every round must produce a result and
//! the next request, until the model answers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolOutput},
    Engine, RunConclusion,
};

/// Counts its calls.
struct Step(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl Tool for Step {
    fn name(&self) -> &str {
        "step"
    }
    fn kernel_capability(&self) -> sven_sdk::tool::ToolCapability {
        sven_sdk::tool::ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Take one step."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"n": {"type": "integer"}}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "ok")
    }
}

/// One round: a call whose arguments arrive in three deltas, and no usage.
fn round(n: usize) -> Vec<ResponseEvent> {
    let delta = |id: &str, name: &str, arguments: &str| ResponseEvent::ToolCall {
        index: 0,
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    };
    vec![
        delta(&format!("s{n}"), "step", "{\"n\""),
        delta("", "", ":"),
        delta("", "", &format!("{n}}}")),
        ResponseEvent::Done,
    ]
}

#[tokio::test]
async fn six_rounds_of_split_calls_without_usage_run_to_the_answer() {
    const ROUNDS: usize = 6;
    let mut scripts: Vec<Vec<ResponseEvent>> = (1..=ROUNDS).map(round).collect();
    scripts.push(vec![
        ResponseEvent::TextDelta("walked".into()),
        ResponseEvent::Done,
    ]);
    let steps = Arc::new(AtomicUsize::new(0));
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .tool(Arc::new(Step(Arc::clone(&steps))))
        .build()
        .expect("an engine builds");

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.agent("agent").send("walk"),
    )
    .await
    .expect("the loop does not stall")
    .expect("an outcome");

    assert_eq!(steps.load(Ordering::SeqCst), ROUNDS);
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert_eq!(outcome.reply, "walked");
}
