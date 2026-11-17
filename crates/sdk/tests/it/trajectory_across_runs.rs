// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an agent driven through several runs keeps the sampled tokens of
//! every request on the step it produced, whatever ended the earlier runs.
//!
//! An application that stops a run from outside - a cancel, or a repetition
//! bound it enforces itself - and then sends the agent its next message must
//! still find the token ids and log-probabilities of the stopped run, and of
//! every run before it, in `Agent::trajectory()`: a learner that trains on
//! the record cannot recover them from anywhere else.
//!
//! Swedish Embedded AB implements training-grade agent trajectories for its
//! clients. If your team needs expertise in recording what a model sampled
//! across a whole agent session then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_sdk::{
    atif::StepOrigin,
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    CancelToken, Engine, RunConclusion, RunOptions,
};
use sven_vocab::SessionEvent;
use tokio::sync::Notify;

struct Probe;

/// A tool the agent mode never lets run: its calls are refused by the gate
/// and answered by the machine, not by the tool.
struct Drive;

#[async_trait::async_trait]
impl Tool for Drive {
    fn name(&self) -> &str {
        "drive"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ControlDevice
    }
    fn description(&self) -> &str {
        "Drives the device."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "driven")
    }
}

#[async_trait::async_trait]
impl Tool for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Reads the probe."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "steady")
    }
}

/// One reply of the scripted server.
enum Reply {
    /// A whole response.
    Answer(Vec<ResponseEvent>),
    /// A response that starts and never finishes; the server says so on
    /// `stalled` so the test can stop the run while it hangs.
    Stall,
}

/// A server that answers each request with the next reply, the way a
/// policy-tracing server would: every whole response carries its prompt and
/// sampled token ids and log-probabilities.
struct Scripted {
    replies: Mutex<VecDeque<Reply>>,
    stalled: Arc<Notify>,
}

#[async_trait::async_trait]
impl ModelProvider for Scripted {
    fn name(&self) -> &str {
        "scripted"
    }
    fn model_name(&self) -> &str {
        "scripted"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let next = self.replies.lock().unwrap().pop_front();
        match next {
            Some(Reply::Answer(events)) => {
                Ok(futures::stream::iter(events.into_iter().map(Ok)).boxed())
            }
            Some(Reply::Stall) => {
                self.stalled.notify_one();
                let first = futures::stream::iter([Ok(ResponseEvent::TextDelta("hm".into()))]);
                Ok(first.chain(futures::stream::pending()).boxed())
            }
            None => anyhow::bail!("the script ran out"),
        }
    }
}

/// The traced tail of every response: the request's token ids are `[id]`.
fn traced(id: i32, mut body: Vec<ResponseEvent>) -> Reply {
    body.push(ResponseEvent::PromptTokens {
        ids: vec![i64::from(1000 + id)],
    });
    body.push(ResponseEvent::Tokens {
        ids: vec![i64::from(id)],
        logprobs: vec![-f64::from(id) / 100.0],
    });
    body.push(ResponseEvent::Usage {
        input_tokens: 10,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: None,
    });
    body.push(ResponseEvent::Done);
    Reply::Answer(body)
}

fn call(id: i32, tool: &str) -> Reply {
    traced(
        id,
        vec![ResponseEvent::ToolCall {
            index: 0,
            id: format!("call-{id}"),
            name: tool.into(),
            arguments: "{}".into(),
        }],
    )
}

fn probe_call(id: i32) -> Reply {
    call(id, "probe")
}

fn answer(id: i32) -> Reply {
    traced(id, vec![ResponseEvent::TextDelta("Done.".into())])
}

fn agent_over(replies: Vec<Reply>) -> (sven_sdk::Agent, Arc<Notify>) {
    let stalled = Arc::new(Notify::new());
    let provider = Scripted {
        replies: Mutex::new(replies.into()),
        stalled: Arc::clone(&stalled),
    };
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .tool(Arc::new(Probe))
        .tool(Arc::new(Drive))
        .build()
        .expect("an engine builds");
    (engine.agent("agent"), stalled)
}

/// The completion ids on the agent's steps, each with its prompt ids and
/// log-probabilities checked against what the server sent for it.
fn traced_ids(agent: &sven_sdk::Agent) -> Vec<i32> {
    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");
    let mut ids = Vec::new();
    for step in trajectory
        .steps
        .iter()
        .filter(|s| s.source == StepOrigin::Agent)
    {
        let Some(metrics) = &step.metrics else {
            continue;
        };
        let Some(completion) = &metrics.completion_token_ids else {
            continue;
        };
        for &id in completion {
            let id = i32::try_from(id).expect("a scripted id");
            ids.push(id);
            assert!(
                metrics
                    .prompt_token_ids
                    .as_ref()
                    .is_some_and(|p| p.contains(&i64::from(1000 + id))),
                "request {id} lost its prompt ids: {metrics:?}"
            );
            assert!(
                metrics
                    .logprobs
                    .as_ref()
                    .is_some_and(|l| l.contains(&(-f64::from(id) / 100.0))),
                "request {id} lost its log-probabilities: {metrics:?}"
            );
        }
    }
    ids
}

/// The trajectory as JSON, for checks on where a call's result sits.
fn trajectory_json(agent: &sven_sdk::Agent) -> String {
    serde_json::to_string(&agent.trajectory()).expect("a trajectory serializes")
}

fn assert_every_call_answered_in_its_step(agent: &sven_sdk::Agent) {
    let trajectory = agent.trajectory();
    for step in &trajectory.steps {
        for invocation in step.tool_calls.iter().flatten() {
            let answered = step.observation.as_ref().is_some_and(|o| {
                o.results
                    .iter()
                    .any(|r| r.source_call_id.as_deref() == Some(invocation.tool_call_id.as_str()))
            });
            assert!(
                answered,
                "call {} has no result in its step: {:#?}",
                invocation.tool_call_id, trajectory.steps
            );
        }
    }
}

fn assert_traced(agent: &sven_sdk::Agent, expected: &[i32]) {
    let found = traced_ids(agent);
    let unique: BTreeSet<i32> = found.iter().copied().collect();
    assert_eq!(
        unique.len(),
        found.len(),
        "a request on two steps: {found:?}"
    );
    for id in expected {
        assert!(
            unique.contains(id),
            "request {id} is on no step; traced: {found:?}"
        );
    }
}

#[tokio::test]
async fn a_run_cancelled_mid_request_keeps_the_traces_of_every_run() {
    let (mut agent, stalled) = agent_over(vec![
        // Run 1: two traced tool calls, then a request the caller cancels.
        probe_call(1),
        probe_call(2),
        Reply::Stall,
        // Run 2 ends cleanly.
        probe_call(4),
        answer(5),
    ]);
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        stalled.notified().await;
        trigger.cancel();
    });
    let first = tokio::time::timeout(
        Duration::from_secs(20),
        agent.send_with("probe it", RunOptions::new().cancel(cancel)),
    )
    .await
    .expect("the cancel stops the run")
    .expect("an outcome");
    assert_eq!(first.conclusion, RunConclusion::Cancelled);
    assert_traced(&agent, &[1, 2]);

    let second = agent.send("probe it again").await.expect("an outcome");
    assert_eq!(second.conclusion, RunConclusion::Success);
    assert_traced(&agent, &[1, 2, 4, 5]);
    assert_every_call_answered_in_its_step(&agent);
}

#[tokio::test]
async fn a_call_the_gate_refuses_keeps_the_traces_of_every_run() {
    let (mut agent, _) = agent_over(vec![
        // Run 1: a call the mode forbids, answered by the machine.
        call(1, "drive"),
        answer(2),
        // Run 2.
        probe_call(3),
        answer(4),
    ]);
    agent.send("drive it").await.expect("an outcome");
    assert_traced(&agent, &[1, 2]);
    agent.send("probe it").await.expect("an outcome");
    assert_traced(&agent, &[1, 2, 3, 4]);
    let json = trajectory_json(&agent);
    assert!(
        json.contains("not run"),
        "the refused call's answer is in the trajectory: {json}"
    );
}

#[tokio::test]
async fn a_run_stopped_by_a_repetition_bound_keeps_the_traces_of_every_run() {
    let (mut agent, _) = agent_over(vec![
        // Run 1 repeats one call; the application stops it after the third
        // identical result, while the model is answering the next request.
        probe_call(1),
        probe_call(2),
        probe_call(3),
        probe_call(4),
        probe_call(6),
        probe_call(7),
        // Run 2 ends cleanly.
        probe_call(8),
        answer(9),
    ]);
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    let mut events = agent.events();
    let watcher = tokio::spawn(async move {
        let mut identical = 0;
        while let Some(event) = events.recv().await {
            if let SessionEvent::ToolCallFinished { tool_name, .. } = &event.event {
                identical += usize::from(tool_name == "probe");
                if identical == 3 {
                    trigger.cancel_with("repetition stop: probe answered identically 3 times");
                    return;
                }
            }
        }
    });
    let first = tokio::time::timeout(
        Duration::from_secs(20),
        agent.send_with("probe it", RunOptions::new().cancel(cancel)),
    )
    .await
    .expect("the repetition stop ends the run")
    .expect("an outcome");
    watcher.await.expect("the watcher ends");
    assert_eq!(first.conclusion, RunConclusion::Cancelled);
    assert_traced(&agent, &[1, 2, 3]);
    assert_every_call_answered_in_its_step(&agent);

    // Whatever the stopped run left unanswered, run 2 gets the next replies.
    let second = agent.send("probe it again").await.expect("an outcome");
    assert!(
        matches!(second.conclusion, RunConclusion::Success),
        "{:?}",
        second.conclusion
    );
    let found = traced_ids(&agent);
    assert_traced(&agent, &[1, 2, 3]);
    assert!(
        found.contains(&9),
        "run 2's answer is on no step: {found:?}"
    );

    // A call the stopped run left unanswered is answered in the step that
    // made it, not after the next message.
    assert_every_call_answered_in_its_step(&agent);
}
