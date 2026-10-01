// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a tool call waits for a person only when the application asked for
//! manual approval.
//!
//! By default every call the agent's mode allows runs. An application with a
//! person, a policy engine or a ticket queue behind it chooses
//! `ApprovalPolicy::Manual` and answers each call's approval as it comes.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in approval workflows for agents
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolOutput},
    ApprovalPolicy, Engine, HumanGate,
};

/// A tool that runs commands (`ExecuteShell`). Records whether it ran.
struct Deploy(Arc<AtomicUsize>);

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
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolOutput::ok(&call.id, "deployed")
    }
}

/// Runs one turn in which the model calls `deploy` twice, under `policy`,
/// with the host answering every approval `answer`. Returns how often the
/// tool ran and the gates the host saw.
async fn deploy_with(policy: ApprovalPolicy, answer: bool) -> (usize, Vec<String>) {
    let deploy = |id: &str| {
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: id.into(),
                name: "deploy".into(),
                arguments: r#"{"env":"prod"}"#.into(),
            },
            ResponseEvent::Done,
        ]
    };
    let provider = ScriptedMockProvider::new(vec![
        deploy("d1"),
        deploy("d2"),
        vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done],
    ]);
    let ran = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .tool(Arc::new(Deploy(Arc::clone(&ran))))
        .approvals(policy)
        .human_gates(move |gate| match gate {
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
        })
        .build()
        .expect("an engine builds");
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        engine.agent("agent").send("ship it"),
    )
    .await
    .expect("the run ends")
    .expect("an outcome");
    let gates = seen.lock().unwrap().clone();
    (ran.load(Ordering::SeqCst), gates)
}

const GATE: &str = r#"ExecuteShell deploy {"env":"prod"}"#;

#[tokio::test]
async fn by_default_a_call_the_mode_allows_runs_without_asking_anyone() {
    let (ran, gates) = deploy_with(ApprovalPolicy::default(), false).await;
    assert!(gates.is_empty(), "nobody was asked: {gates:?}");
    assert_eq!(ran, 2, "the mode allows shell, so both calls ran");
}

#[tokio::test]
async fn under_manual_approval_each_call_is_put_to_the_host_and_runs_once_approved() {
    let (ran, gates) = deploy_with(ApprovalPolicy::Manual, true).await;
    assert_eq!(
        gates,
        [GATE, GATE],
        "asked once per call, not once per capability"
    );
    assert_eq!(ran, 2, "approved, so both ran");
}

#[tokio::test]
async fn under_manual_approval_a_refused_call_never_runs() {
    let (ran, gates) = deploy_with(ApprovalPolicy::Manual, false).await;
    assert_eq!(gates, [GATE, GATE]);
    assert_eq!(ran, 0, "refused, so it never ran");
}

/// Manual approval with nobody to approve fails when the engine is built,
/// rather than hanging at the first call or approving on nobody's behalf.
#[test]
fn manual_approval_without_a_handler_does_not_build() {
    let err = Engine::builder()
        .approvals(ApprovalPolicy::Manual)
        .build()
        .err()
        .expect("refused");
    assert!(
        matches!(&err, sven_sdk::CallError::Precondition(m) if m.contains("human_gates")),
        "{err}"
    );
}
