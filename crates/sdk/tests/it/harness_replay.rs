// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness run can be recorded and replayed offline. The recording
//! is the machine's input event log; replaying it re-dispatches the log
//! into a fresh machine and executes no effect - no model is asked, no tool
//! runs - yet reaches the same verdict and trace. A checkpoint survives
//! JSON, a run's steps can be counted from its trace, and its conversation
//! exports as an ATIF trajectory.
//!
//! Swedish Embedded AB implements deterministic, auditable agent runtimes
//! for its clients. If your team needs expertise in reproducing and
//! debugging autonomous workflows then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_hsm::Event;
use sven_model::ResponseEvent;
use sven_model_mock::{FailingMockProvider, ScriptedMockProvider};
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    ApprovalPolicy, Engine, Harness, HarnessCheckpoint, HarnessRecording, HumanGate, RunConclusion,
    RunOptions,
};
use tokio::sync::oneshot;

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

fn engine_with(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)) as Arc<_>)
        .build()
        .expect("an engine builds from a default config")
}

/// An engine whose model always fails: anything that asked it would show.
fn engine_without_a_model() -> Engine {
    Engine::builder()
        .model_provider(Arc::new(FailingMockProvider::new("replay must not ask")) as Arc<_>)
        .build()
        .expect("an engine builds")
}

const GREET: &str = r#"
api_version: "2"
name: greet
inputs:
  topic: string
execution: { limits: { model_turns: 4 } }
workflow:
  - name: ask
    model_only: true
    task: "Name one word about {{ inputs.topic }}."
outputs:
  topic: { type: string, value: "{{ inputs.topic }}" }
  word: { type: string, value: "{{ tasks.ask.output }}" }
"#;

/// The same name with another prompt: a different program.
const GREET_EDITED: &str = r#"
api_version: "2"
name: greet
inputs:
  topic: string
execution: { limits: { model_turns: 4 } }
workflow:
  - name: ask
    model_only: true
    task: "Name two words about {{ inputs.topic }}."
outputs:
  topic: { type: string, value: "{{ inputs.topic }}" }
  word: { type: string, value: "{{ tasks.ask.output }}" }
"#;

/// The same question put to an agent, whose conversation is the run's own.
const AGENT_GREET: &str = r#"
api_version: "2"
name: agent-greet
inputs:
  topic: string
execution: { limits: { model_turns: 4 } }
workflow:
  - name: ask
    task: "Name one word about {{ inputs.topic }}."
"#;

const GATED: &str = r#"
api_version: "2"
name: gated
execution: { limits: { model_turns: 4 } }
gates:
  open: { command: [open-the-gate], timeout: 30 }
workflow:
  - name: wait
    verify: [open]
outputs:
  answer: { type: string, value: "{{ gates.open.stdout }}" }
"#;

const WRITES: &str = r#"
api_version: "2"
name: writes
agent: { profile: coding, tools: [write_notes] }
execution: { limits: { model_turns: 4, tool_calls: 2 } }
workflow:
  - name: save
    task: Save the notes.
"#;

/// The gate tool a workflow's gates call, scripted: its first call blocks
/// until `release` is opened (and announces itself on `started`), every later
/// call answers at once.
struct Gate {
    started: Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Tool for Gate {
    fn name(&self) -> &str {
        "shell_gate"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    fn description(&self) -> &str {
        "answers once it is opened"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            if let Some(started) = self.started.lock().expect("gate lock").take() {
                let _ = started.send(());
            }
            self.release.notified().await;
        }
        ToolOutput::ok(
            &call.id,
            json!({ "exit_code": 0, "timed_out": false, "error": null,
                    "stdout": "{\"pass\": true}", "stderr": "" })
            .to_string(),
        )
    }
}

fn gated_engine(started: oneshot::Sender<()>, release: Arc<tokio::sync::Notify>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(vec![])) as Arc<_>)
        .tool(Arc::new(Gate {
            started: Mutex::new(Some(started)),
            release,
            calls: AtomicUsize::new(0),
        }))
        .build()
        .expect("the engine takes the tool")
}

/// Runs `harness` to its end with recording on; returns the outcome and
/// the recording.
async fn recorded(
    harness: &Harness,
    inputs: Value,
) -> (sven_sdk::HarnessOutcome, HarnessRecording) {
    let mut run = harness
        .start(inputs)
        .expect("inputs check out")
        .with_recording();
    let outcome = run.execute(RunOptions::new()).await.expect("the run ends");
    let recording = run.recording().expect("a recorded run has a recording");
    (outcome, recording)
}

/// The recording as a host would store it: through JSON and back.
fn through_json(recording: &HarnessRecording) -> HarnessRecording {
    let text = serde_json::to_string(recording).expect("a recording serializes");
    serde_json::from_str(&text).expect("a recording deserializes")
}

#[tokio::test]
async fn a_recorded_run_replays_to_the_same_outcome_and_trace() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(GREET)
        .expect("loads");
    let (outcome, recording) = recorded(&harness, json!({"topic": "reliability"})).await;

    assert_eq!(recording.format, "sven-harness-recording/1");
    assert_eq!(recording.harness, "greet");
    assert_eq!(recording.plan_digest, harness.digest());
    assert!(recording.start.is_none(), "a fresh run has no start");
    assert!(!recording.events.is_empty());

    let replay = harness
        .replay(&through_json(&recording))
        .expect("a recording replays under its own program");
    assert_eq!(replay.state, "Completed");
    assert_eq!(replay.trace, outcome.trace);
    assert_eq!(replay.steps, outcome.steps);
    assert_eq!(
        replay.result.as_ref().map(|r| r.as_ref().ok()),
        outcome.result.as_ref().map(|r| r.as_ref().ok()),
        "the same verdict"
    );
    assert!(matches!(&replay.result, Some(Ok(v)) if v["word"] == json!("resilience")));
}

#[tokio::test]
async fn two_replays_of_one_recording_are_identical() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(GREET)
        .expect("loads");
    let (_, recording) = recorded(&harness, json!({"topic": "reliability"})).await;

    let first = harness.replay(&recording).expect("replays");
    let second = harness.replay(&recording).expect("replays");
    assert_eq!(first.state, second.state);
    assert_eq!(first.trace, second.trace);
    assert_eq!(first.steps, second.steps);
    assert_eq!(first.effects, second.effects);
    assert!(
        first.result.is_some() && format!("{:?}", first.result) == format!("{:?}", second.result)
    );
}

#[tokio::test]
async fn replay_executes_nothing() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(GREET)
        .expect("loads");
    let (outcome, recording) = recorded(&harness, json!({"topic": "reliability"})).await;

    // The same program on an engine with a model that fails if asked and no
    // tools at all: replay still completes, because it asks nobody.
    let offline = engine_without_a_model()
        .load_harness_from_str(GREET)
        .expect("loads");
    let replay = offline.replay(&recording).expect("replays offline");
    assert_eq!(replay.trace, outcome.trace);
    assert!(matches!(&replay.result, Some(Ok(_))), "{:?}", replay.result);
    assert!(
        !replay.effects.is_empty(),
        "the effects the machine emitted are reported, never executed"
    );
}

#[tokio::test]
async fn a_recording_is_refused_by_a_changed_program() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(GREET)
        .expect("loads");
    let (_, recording) = recorded(&harness, json!({"topic": "reliability"})).await;

    let edited = engine_with(vec![])
        .load_harness_from_str(GREET_EDITED)
        .expect("the edited program loads on its own engine");
    let error = edited
        .replay(&recording)
        .expect_err("a changed program must not adopt the recording");
    assert!(
        matches!(error, sven_sdk::CallError::Precondition(_)),
        "{error:?}"
    );

    let mut forged = recording.clone();
    forged.bundle_digest = Some("sha256:not-this-bundle".to_string());
    assert!(
        harness.replay(&forged).is_err(),
        "a recording pinned to a bundle is refused by a bundle-less harness"
    );
}

#[tokio::test]
async fn a_resumed_runs_recording_replays_from_its_start_checkpoint() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, Arc::clone(&release));
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let mut first = harness.start(json!({})).expect("starts").with_recording();
    let worker = tokio::spawn(async move {
        first
            .execute(RunOptions::new().deadline(std::time::Duration::from_millis(200)))
            .await
            .expect("the run stops on the bound");
        first.checkpoint()
    });
    started_rx.await.expect("the run parks on its tool");
    let checkpoint = worker
        .await
        .expect("ends")
        .expect("a suspended run checkpoints");
    release.notify_waiters();

    let mut second = harness
        .resume(checkpoint.clone())
        .expect("the same program resumes")
        .with_recording();
    let outcome = second.execute(RunOptions::new()).await.expect("ends");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    let recording = second.recording().expect("recorded");
    let start = recording
        .start
        .as_ref()
        .expect("a resumed run starts from its checkpoint");
    assert_eq!(start.run_id, checkpoint.run_id);

    let replay = harness
        .replay(&through_json(&recording))
        .expect("replays from the checkpoint");
    assert_eq!(replay.state, "Completed");
    assert_eq!(replay.trace, outcome.trace);
    assert_eq!(replay.steps, outcome.steps);
    assert!(
        replay
            .trace
            .iter()
            .any(|r| matches!(r, sven_sdk::TraceRecord::GateEvaluated { passed: true, .. })),
        "the interrupted gate ran again and passed: {:?}",
        replay.trace
    );
}

#[tokio::test]
async fn a_checkpoint_survives_json_and_resumes() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, Arc::clone(&release));
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let mut run = harness.start(json!({})).expect("starts");
    let worker = tokio::spawn(async move {
        run.execute(RunOptions::new().deadline(std::time::Duration::from_millis(200)))
            .await
            .expect("the run stops on the bound");
        run.checkpoint()
    });
    started_rx.await.expect("the run parks on its tool");
    let checkpoint = worker.await.expect("ends").expect("checkpoints");
    release.notify_waiters();

    let text = serde_json::to_string(&checkpoint).expect("a checkpoint serializes");
    let restored: HarnessCheckpoint = serde_json::from_str(&text).expect("and reads back");
    assert_eq!(restored.plan_digest, harness.digest());
    assert_eq!(restored.run_id, checkpoint.run_id);

    let outcome = harness
        .resume(restored)
        .expect("resumes")
        .execute(RunOptions::new())
        .await
        .expect("ends");
    assert!(matches!(&outcome.result, Some(Ok(v)) if v == &json!({"answer": "{\"pass\": true}"})));
}

#[tokio::test]
async fn step_counts_follow_the_trace() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(GREET)
        .expect("loads");
    let (outcome, recording) = recorded(&harness, json!({"topic": "reliability"})).await;

    let counts = outcome.step_counts();
    assert_eq!(counts.get("ask__work"), Some(&1), "{counts:?}");
    assert_eq!(counts.get("complete"), Some(&1), "{counts:?}");

    let replay = harness.replay(&recording).expect("replays");
    assert_eq!(
        replay.step_counts(),
        counts,
        "a replay counts the same steps"
    );
}

#[tokio::test]
async fn a_run_exports_its_conversation_as_a_trajectory() {
    let harness = engine_with(vec![says("resilience")])
        .load_harness_from_str(AGENT_GREET)
        .expect("loads");
    let mut run = harness
        .start(json!({"topic": "reliability"}))
        .expect("starts");
    assert!(run.trajectory().is_none(), "nothing ran yet");
    let outcome = run.execute(RunOptions::new()).await.expect("ends");

    let trajectory = run
        .trajectory()
        .expect("an executed run exports a trajectory");
    // The conversation of a workflow travels with the trajectory, under its own name.
    let text = serde_json::to_string(&trajectory).expect("the trajectory serializes");
    assert!(text.contains("Name one word about reliability"), "{text}");
    assert!(text.contains("resilience"), "{text}");

    let harness_extra = &trajectory.extra.as_ref().expect("extra")["harness"];
    assert_eq!(harness_extra["name"], json!("agent-greet"));
    assert_eq!(harness_extra["plan_digest"], json!(harness.digest()));
    assert_eq!(harness_extra["run_id"], json!(run.run_id()));
    assert_eq!(
        harness_extra["trace"].as_array().map(Vec::len),
        Some(outcome.trace.len()),
        "the structured trace travels with the conversation"
    );
    let metrics = trajectory.final_metrics.as_ref().expect("final metrics");
    assert_eq!(metrics.total_steps, Some(outcome.steps));
}

#[tokio::test]
async fn a_recording_captures_the_human_approval_events() {
    let runs = Arc::new(AtomicUsize::new(0));
    struct Writer(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl Tool for Writer {
        fn name(&self) -> &str {
            "write_notes"
        }
        fn kernel_capability(&self) -> ToolCapability {
            ToolCapability::WriteFile
        }
        fn description(&self) -> &str {
            "writes notes"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        fn default_policy(&self) -> ToolApprovalPolicy {
            ToolApprovalPolicy::Auto
        }
        async fn execute(&self, call: &ToolCall) -> ToolOutput {
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolOutput::ok(&call.id, "done")
        }
    }
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::tool_then_text(
            "w1",
            "write_notes",
            "{}",
            "saved",
        )) as Arc<_>)
        .approvals(ApprovalPolicy::Manual)
        .human_gates(|gate| match gate {
            HumanGate::Approval { reply_tx, .. } => {
                let _ = reply_tx.send(true);
            }
            HumanGate::Question { reply_tx, .. } => {
                let _ = reply_tx.send(String::new());
            }
        })
        .tool(Arc::new(Writer(Arc::clone(&runs))))
        .build()
        .expect("an engine builds");
    let harness = engine.load_harness_from_str(WRITES).expect("loads");

    let (outcome, recording) = recorded(&harness, json!({})).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "approved, so it ran once");
    assert!(
        recording
            .events
            .iter()
            .any(|event| matches!(event, Event::HumanApproved { .. })),
        "the host's answer is part of the log: {:?}",
        recording.events
    );

    let replay = harness.replay(&through_json(&recording)).expect("replays");
    assert_eq!(replay.trace, outcome.trace);
    assert_eq!(runs.load(Ordering::SeqCst), 1, "replay ran no tool");
}
