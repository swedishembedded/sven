// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an exported trajectory says what the permission gate decided about
//! each call, who decided it, whether the arguments fit the tool's schema, and
//! what memory the call read.
//!
//! A reader of the trajectory (an auditor, a learning pipeline) must not have
//! to guess whether a call ran because a person said yes, because a policy
//! allowed it, or because nobody was there to refuse it.
//!
//! Swedish Embedded AB implements auditable agent runtimes for its clients. If
//! your team needs expertise in recording agent decisions for audit and
//! learning then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::atif::{
    validate_trajectory, GateAuthority, GateOutcome, GateRecord, MemoryAnchor, MemoryRecord,
    MemoryResult, MemoryRetrieval,
};
use sven_sdk::tool::{
    record_memory_retrieval, ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability,
    ToolOutput,
};
use sven_sdk::{Agent, ApprovalPolicy, Engine, HumanGate};

/// A shell-capable tool that requires an `env` string.
struct Deploy;

#[async_trait::async_trait]
impl Tool for Deploy {
    fn name(&self) -> &str {
        "deploy"
    }
    fn description(&self) -> &str {
        "Deploy to an environment."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {"env": {"type": "string"}},
            "required": ["env"]
        })
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "deployed")
    }
}

/// A read-only tool that reports the memory it read through the recording API.
struct Recall;

#[async_trait::async_trait]
impl Tool for Recall {
    fn name(&self) -> &str {
        "recall"
    }
    fn description(&self) -> &str {
        "Look something up in memory."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let recorded = record_memory_retrieval(MemoryRetrieval {
            call_id: String::new(),
            query: "retry policy".into(),
            space_ids: vec!["space-1".into()],
            generation_id: "gen-a".into(),
            snapshot_id: None,
            results: vec![MemoryResult {
                unit_id: "unit-7".into(),
                revision: "sha256:abc".into(),
                anchor: MemoryAnchor {
                    heading_path: vec!["Retries".into()],
                    block_ordinal: 2,
                    text_fingerprint: "f00d".into(),
                },
                score: 0.83,
                rank: 1,
                inserted: true,
            }],
        });
        assert!(recorded, "the tool runs under an executor");
        ToolOutput::ok(&call.id, "retry three times")
    }
}

fn calls(id: &str, tool: &str, arguments: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: tool.into(),
            arguments: arguments.into(),
        },
        ResponseEvent::Done,
    ]
}

fn says_ok() -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
}

/// What a host does with an approval.
#[derive(Clone, Copy)]
enum Host {
    Approves,
    Refuses,
    DropsTheGate,
}

fn engine(scripts: Vec<Vec<ResponseEvent>>, policy: ApprovalPolicy, host: Host) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .tool(Arc::new(Deploy))
        .tool(Arc::new(Recall))
        .approvals(policy)
        .human_gates(move |gate| match gate {
            HumanGate::Approval { reply_tx, .. } => match host {
                Host::Approves => {
                    let _ = reply_tx.send(true);
                }
                Host::Refuses => {
                    let _ = reply_tx.send(false);
                }
                Host::DropsTheGate => drop(reply_tx),
            },
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(String::new());
            }
        })
        .build()
        .expect("an engine builds")
}

async fn run(engine: &Engine) -> Agent {
    let mut agent = engine.agent("agent");
    tokio::time::timeout(std::time::Duration::from_secs(20), agent.send("go"))
        .await
        .expect("the run ends")
        .expect("an outcome");
    agent
}

/// The trajectory of `agent`, validated, with every gate record in it.
fn gate_decisions(agent: &Agent) -> Vec<sven_sdk::atif::GateDecision> {
    let trajectory = agent.trajectory();
    assert_eq!(validate_trajectory(&trajectory), Ok(()));
    trajectory
        .steps
        .iter()
        .filter_map(|s| GateRecord::from_extra(s.extra.as_ref()?))
        .flat_map(|r| r.decisions)
        .collect()
}

#[tokio::test]
async fn a_person_who_approves_or_refuses_is_recorded_against_the_call() {
    let scripts = vec![calls("d1", "deploy", r#"{"env":"prod"}"#), says_ok()];
    let approved = run(&engine(scripts, ApprovalPolicy::Manual, Host::Approves)).await;
    let decisions = gate_decisions(&approved);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].call_id, "d1");
    assert_eq!(decisions[0].outcome, GateOutcome::Approved);
    assert_eq!(decisions[0].decided_by, GateAuthority::Human);
    assert_eq!(decisions[0].capability, "ExecuteShell");
    assert_eq!(decisions[0].policy, "requires_approval");
    assert!(!decisions[0].decision_id.is_empty());

    let scripts = vec![calls("d2", "deploy", r#"{"env":"prod"}"#), says_ok()];
    let refused = run(&engine(scripts, ApprovalPolicy::Manual, Host::Refuses)).await;
    let decisions = gate_decisions(&refused);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].call_id, "d2");
    assert_eq!(decisions[0].outcome, GateOutcome::Denied);
    assert_eq!(decisions[0].decided_by, GateAuthority::Human);
}

#[tokio::test]
async fn a_gate_dropped_without_an_answer_is_recorded_as_handler_dropped() {
    let scripts = vec![calls("d1", "deploy", r#"{"env":"prod"}"#), says_ok()];
    let agent = run(&engine(scripts, ApprovalPolicy::Manual, Host::DropsTheGate)).await;
    let decisions = gate_decisions(&agent);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].outcome, GateOutcome::Denied);
    assert_eq!(decisions[0].decided_by, GateAuthority::HandlerDropped);
    assert_eq!(decisions[0].reason.as_deref(), Some("handler_dropped"));
}

#[tokio::test]
async fn a_call_the_policy_never_asked_anyone_about_has_no_gate_record() {
    let scripts = vec![calls("d1", "deploy", r#"{"env":"prod"}"#), says_ok()];
    let agent = run(&engine(scripts, ApprovalPolicy::default(), Host::Refuses)).await;
    assert!(gate_decisions(&agent).is_empty());
}

#[tokio::test]
async fn a_schema_invalid_call_is_recorded_with_a_typed_violation_and_still_reaches_the_tool() {
    let scripts = vec![calls("d1", "deploy", r#"{"env":7}"#), says_ok()];
    let agent = run(&engine(scripts, ApprovalPolicy::default(), Host::Refuses)).await;
    let decisions = gate_decisions(&agent);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].call_id, "d1");
    assert_eq!(decisions[0].decided_by, GateAuthority::Policy);
    assert_eq!(decisions[0].policy, "allowed");
    let violation = &decisions[0].violations[0];
    assert_eq!(
        (violation.path.as_str(), violation.rule.as_str()),
        ("/env", "type")
    );

    let json = serde_json::to_string(&agent.trajectory()).unwrap();
    assert!(
        json.contains("deployed"),
        "the call was not blocked by the check"
    );
}

#[tokio::test]
async fn a_violation_on_an_approved_call_joins_the_humans_decision() {
    let scripts = vec![calls("d1", "deploy", "{}"), says_ok()];
    let agent = run(&engine(scripts, ApprovalPolicy::Manual, Host::Approves)).await;
    let decisions = gate_decisions(&agent);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].decided_by, GateAuthority::Human);
    assert_eq!(decisions[0].violations[0].rule, "required");
}

#[tokio::test]
async fn memory_use_is_recorded_with_unit_ids_and_generation() {
    let scripts = vec![calls("r1", "recall", "{}"), says_ok()];
    let agent = run(&engine(scripts, ApprovalPolicy::default(), Host::Refuses)).await;

    let trajectory = agent.trajectory();
    assert_eq!(validate_trajectory(&trajectory), Ok(()));
    let record = trajectory
        .steps
        .iter()
        .find_map(|s| MemoryRecord::from_extra(s.extra.as_ref()?))
        .expect("a memory record");
    assert_eq!(record.retrievals.len(), 1);
    let retrieval = &record.retrievals[0];
    assert_eq!(retrieval.call_id, "r1", "the executor names the call");
    assert_eq!(retrieval.generation_id, "gen-a");
    assert_eq!(retrieval.results[0].unit_id, "unit-7");
}

#[tokio::test]
async fn the_records_survive_suspend_and_resume() {
    let scripts = vec![calls("d1", "deploy", "{}"), says_ok()];
    let engine = engine(scripts, ApprovalPolicy::Manual, Host::Approves);
    let agent = run(&engine).await;
    let state = serde_json::to_string(agent.state()).expect("the state serializes");

    let resumed = engine
        .resume(serde_json::from_str(&state).expect("the state reads"))
        .expect("it resumes");
    let decisions = gate_decisions(&resumed);
    assert_eq!(decisions.len(), 1, "{decisions:?}");
    assert_eq!(decisions[0].violations.len(), 1);
}
