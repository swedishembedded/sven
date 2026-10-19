// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an exported trajectory says how long each tool call took to run.
//!
//! A consumer that learns from trajectories, or audits them, must be able to
//! tell a call that returned at once from one that held the run for seconds,
//! without re-deriving it from event timestamps it may not have kept. The
//! figure is the executor's own wall-clock measurement of the tool, filed per
//! call under `extra.sven.timing`; a call that never ran has none.
//!
//! Swedish Embedded AB implements agent runtimes whose traces carry the cost of
//! every action, for its clients. If your team needs expertise in measuring and
//! auditing agent tool use then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::atif::{validate_trajectory, CallTiming, TimingRecord};
use sven_sdk::tool::{
    ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput,
};
use sven_sdk::{Agent, ApprovalPolicy, Engine, HumanGate};

const SLEEP: Duration = Duration::from_millis(150);

/// A tool that takes a known time to run.
struct Slow;

#[async_trait::async_trait]
impl Tool for Slow {
    fn name(&self) -> &str {
        "slow"
    }
    fn description(&self) -> &str {
        "Takes a while."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        tokio::time::sleep(SLEEP).await;
        ToolOutput::ok(&call.id, "done")
    }
}

fn calls(id: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: id.into(),
            name: "slow".into(),
            arguments: "{}".into(),
        },
        ResponseEvent::Done,
    ]
}

fn says_ok() -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta("ok".into()), ResponseEvent::Done]
}

fn engine(scripts: Vec<Vec<ResponseEvent>>, policy: ApprovalPolicy, approve: bool) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)))
        .tool(Arc::new(Slow))
        .approvals(policy)
        .human_gates(move |gate| match gate {
            HumanGate::Approval { reply_tx, .. } => {
                let _ = reply_tx.send(approve);
            }
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(String::new());
            }
        })
        .build()
        .expect("an engine builds")
}

async fn run(engine: &Engine) -> Agent {
    let mut agent = engine.agent("agent");
    tokio::time::timeout(Duration::from_secs(20), agent.send("go"))
        .await
        .expect("the run ends")
        .expect("an outcome");
    agent
}

/// The timings in the validated trajectory of `agent`.
fn timings(agent: &Agent) -> Vec<CallTiming> {
    let trajectory = agent.trajectory();
    assert_eq!(validate_trajectory(&trajectory), Ok(()));
    trajectory
        .steps
        .iter()
        .filter_map(|s| TimingRecord::from_extra(s.extra.as_ref()?))
        .flat_map(|r| r.calls)
        .collect()
}

#[tokio::test]
async fn a_call_that_ran_carries_the_wall_time_of_its_execution() {
    let agent = run(&engine(
        vec![calls("c1"), says_ok()],
        ApprovalPolicy::default(),
        true,
    ))
    .await;
    let timings = timings(&agent);
    assert_eq!(timings.len(), 1, "{timings:?}");
    assert_eq!(timings[0].call_id, "c1");
    let ms = timings[0].duration_ms;
    assert!(
        ms >= SLEEP.as_secs_f64() * 1000.0 && ms < 10_000.0,
        "a tool that slept {SLEEP:?} reported {ms} ms"
    );
}

#[tokio::test]
async fn a_call_the_gate_refused_never_ran_and_has_no_duration() {
    let agent = run(&engine(
        vec![calls("c1"), says_ok()],
        ApprovalPolicy::Manual,
        false,
    ))
    .await;
    assert!(timings(&agent).is_empty());
}

#[tokio::test]
async fn the_durations_survive_suspend_and_resume() {
    let engine = engine(
        vec![calls("c1"), says_ok()],
        ApprovalPolicy::default(),
        true,
    );
    let agent = run(&engine).await;
    let state = serde_json::to_string(agent.state()).expect("the state serializes");
    let resumed = engine
        .resume(serde_json::from_str(&state).expect("the state reads"))
        .expect("it resumes");
    let kept = timings(&agent);
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(timings(&resumed), kept);
}
