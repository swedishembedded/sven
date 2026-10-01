// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an application can answer the human gate itself.
//!
//! Denying everything or approving everything are both decisions made on
//! behalf of a person who is not there. An application with a person, a
//! policy engine or a ticket queue behind it answers each gate as it comes.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in approval workflows for agents
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    ApprovalPolicy, Engine, HumanGate,
};

/// A tool that runs commands (`ExecuteShell`). Records whether it ran.
struct Deploy(Arc<AtomicBool>);

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
        sven_sdk::tool::ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.0.store(true, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "deployed")
    }
}

/// Runs one turn in which the model calls `deploy`, with `answer` deciding
/// the gate. Returns whether the tool ran and the gates the host saw.
async fn deploy_with(answer: bool) -> (bool, Vec<String>) {
    let provider = ScriptedMockProvider::new(vec![
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: "d1".into(),
                name: "deploy".into(),
                arguments: r#"{"env":"prod"}"#.into(),
            },
            ResponseEvent::Done,
        ],
        vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done],
    ]);
    let ran = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .tool(Arc::new(Deploy(Arc::clone(&ran))))
        .approvals(ApprovalPolicy::ask(move |gate| match gate {
            HumanGate::Approval {
                capability,
                call,
                reply_tx,
                ..
            } => {
                let call = call.expect("a tool approval names the call it gates");
                log.lock()
                    .unwrap()
                    .push(format!("{capability:?} {} {}", call.name, call.args));
                let _ = reply_tx.send(answer);
            }
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(String::new());
            }
        }))
        .build()
        .expect("an engine builds");
    engine
        .agent("agent")
        .send("ship it")
        .await
        .expect("an outcome");
    let gates = seen.lock().unwrap().clone();
    (ran.load(Ordering::SeqCst), gates)
}

#[tokio::test]
async fn a_call_the_mode_allows_runs_without_asking_anyone() {
    let (ran, gates) = deploy_with(false).await;
    assert!(gates.is_empty(), "nobody was asked: {gates:?}");
    assert!(ran, "the mode allows shell, so it ran");
}
