// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the SDK harness contract - load a YAML-defined harness on an
//! engine, start a run, execute it to a typed outcome, stop it on a bound,
//! and resume the checkpoint under the same program.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in building high-throughput services on top of
//! agent state machines then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::sync::Arc;

use serde_json::{json, Value};
use sven_harness_runtime::TraceRecord;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    CallError, CancelToken, Engine, RunConclusion, RunOptions,
};
use tokio::sync::oneshot;

/// One scripted assistant reply.
fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

/// An engine wired to a scripted provider.
fn engine_with(scripts: Vec<Vec<ResponseEvent>>) -> (Engine, Arc<ScriptedMockProvider>) {
    let provider = Arc::new(ScriptedMockProvider::new(scripts));
    let engine = Engine::builder()
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds from a default config");
    (engine, provider)
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

/// A run whose own check refuses: the gate exits 1.
const FAILING: &str = r#"
api_version: "2"
name: refusing
execution: { limits: { model_turns: 4 } }
gates:
  ready: { command: [ready] }
workflow:
  - name: check
    verify: [ready]
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

const GATED_EDITED: &str = r#"
api_version: "2"
name: gated
execution: { limits: { model_turns: 4 } }
gates:
  open: { command: [open-the-gate, --again], timeout: 30 }
workflow:
  - name: wait
    verify: [open]
outputs:
  answer: { type: string, value: "{{ gates.open.stdout }}" }
"#;

/// The gate tool a workflow's gates call, scripted: its first call blocks
/// until `release` is opened (and announces itself on `started`), every later
/// call answers at once with `exit_code`.
struct Gate {
    started: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    exit_code: i64,
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
        let call_number = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call_number == 0 {
            if let Some(started) = self.started.lock().expect("gate lock").take() {
                let _ = started.send(());
            }
            self.release.notified().await;
        }
        ToolOutput::ok(
            &call.id,
            json!({ "exit_code": self.exit_code, "timed_out": false, "error": null,
                    "stdout": "{\"pass\": true}", "stderr": "" })
            .to_string(),
        )
    }
}

/// An engine carrying the `gate` tool, so a harness can park on its call.
fn gated_engine(started_tx: oneshot::Sender<()>, release: Arc<tokio::sync::Notify>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(vec![])) as Arc<_>)
        .tool(Arc::new(Gate {
            started: std::sync::Mutex::new(Some(started_tx)),
            release,
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            exit_code: 0,
        }))
        .build()
        .expect("the engine takes the tool")
}

/// An engine whose gate tool answers with exit status 1.
fn failing_engine(started_tx: oneshot::Sender<()>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(vec![])) as Arc<_>)
        .tool(Arc::new(Gate {
            started: std::sync::Mutex::new(Some(started_tx)),
            release: Arc::new(tokio::sync::Notify::new()),
            calls: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
            exit_code: 1,
        }))
        .build()
        .expect("the engine takes the tool")
}

#[tokio::test]
async fn a_harness_runs_to_its_return_value() {
    let (engine, _p) = engine_with(vec![says("resilience")]);
    let harness = engine
        .load_harness_from_str(GREET)
        .expect("a valid harness loads");
    assert_eq!(harness.name(), "greet");
    assert!(!harness.digest().is_empty());

    let mut run = harness
        .start(json!({"topic": "reliability"}))
        .expect("inputs check out");
    let outcome = run.execute(RunOptions::new()).await.expect("the run ends");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert!(
        matches!(&outcome.result, Some(Ok(value))
            if value == &json!({"topic": "reliability", "word": "resilience"})),
        "the run returns the plan's value, got {:?}",
        outcome.result
    );
    assert!(
        outcome.trace.iter().any(
            |record| matches!(record, TraceRecord::StepCompleted { step, .. } if step == "ask__work")
        ),
        "the trace records the step that ran: {:?}",
        outcome.trace
    );
    assert!(
        run.checkpoint().is_none(),
        "a finished run has nothing to resume"
    );
}

#[tokio::test]
async fn inputs_are_validated_before_anything_runs() {
    let (engine, provider) = engine_with(vec![says("unused")]);
    let harness = engine.load_harness_from_str(GREET).expect("loads");

    let error = match harness.start(json!({})) {
        Err(error) => error,
        Ok(_) => panic!("a missing input is refused"),
    };
    assert!(
        matches!(&error, CallError::Precondition(detail) if detail.contains("topic")),
        "the refusal names the violated field: {error:?}"
    );
    assert!(
        provider.last_request.lock().unwrap().is_none(),
        "nothing ran: the model was never asked"
    );
}

#[tokio::test]
async fn a_failed_harness_reports_its_own_failure() {
    let (started_tx, _started_rx) = oneshot::channel();
    let engine = failing_engine(started_tx);
    let harness = engine.load_harness_from_str(FAILING).expect("loads");

    let mut run = harness.start(json!({})).expect("starts");
    let outcome = run.execute(RunOptions::new()).await.expect("ends");
    assert_eq!(
        outcome.conclusion,
        RunConclusion::AgentError,
        "a failed run is not a success"
    );
    assert!(
        matches!(&outcome.result, Some(Err(failure)) if failure.code == "gate_failed"),
        "the plan's own failure is the verdict, got {:?}",
        outcome.result
    );
}

#[tokio::test]
async fn a_bounded_run_checkpoints_and_resumes_under_the_same_program() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, Arc::clone(&release));
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let mut run = harness.start(json!({})).expect("starts");
    let run_id = run.run_id().to_string();
    let worker = tokio::spawn(async move {
        let outcome = run
            // A deadline, not a cancel: the bound suspends the run where it
            // rests, and the checkpoint is what a resume continues.
            .execute(RunOptions::new().deadline(std::time::Duration::from_millis(200)))
            .await
            .expect("the run stops on the bound");
        (outcome, run.checkpoint())
    });

    // The tool call is in flight: the run is parked on it. Let the bound
    // stop it there.
    started_rx.await.expect("the run reached its tool step");
    let (outcome, checkpoint) = worker.await.expect("the worker ends");

    assert_eq!(outcome.conclusion, RunConclusion::Timeout);
    assert!(
        outcome.result.is_none(),
        "a suspended run has no verdict: {:?}",
        outcome.result
    );
    let checkpoint = checkpoint.expect("a suspend on a parked run leaves a checkpoint");
    assert_eq!(checkpoint.plan_digest, harness.digest());
    assert_eq!(checkpoint.run_id, run_id, "the checkpoint names its run");

    // The abandoned call ended with the suspension. The resumed run asks the
    // gate again - without spending a repair attempt on the lost answer -
    // and the second call is answered at once.
    release.notify_waiters();
    let mut second = harness
        .resume(checkpoint)
        .expect("the same program resumes");
    let outcome = second
        .execute(RunOptions::new())
        .await
        .expect("the run ends");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert!(
        matches!(&outcome.result, Some(Ok(value)) if value == &json!({"answer": "{\"pass\": true}"})),
        "the resumed run finishes the plan, got {:?}",
        outcome.result
    );
    assert!(
        outcome
            .trace
            .iter()
            .any(|record| matches!(record, TraceRecord::GateEvaluated { passed: true, .. })),
        "the interrupted gate ran again and passed: {:?}",
        outcome.trace
    );
    // The resume keeps the run id: both episodes' events read as one
    // run's.
    assert_eq!(second.run_id(), run_id, "a resume keeps the run id");
}

#[tokio::test]
async fn a_run_suspended_by_the_host_leaves_a_checkpoint_a_resume_completes() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, Arc::clone(&release));
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let mut run = harness.start(json!({})).expect("starts");
    let stop = sven_sdk::CancelToken::new();
    let token = stop.clone();
    let worker = tokio::spawn(async move {
        // No deadline: the host's own request (a signal, say) suspends the
        // run where it rests, unlike a cancel, which finishes it failed.
        let outcome = run
            .execute(RunOptions::new().suspend_on(token))
            .await
            .expect("the run stops on the request");
        (outcome, run.checkpoint())
    });

    started_rx.await.expect("the run reached its tool step");
    stop.cancel();
    let (outcome, checkpoint) = worker.await.expect("the worker ends");

    assert_eq!(outcome.conclusion, RunConclusion::Timeout);
    assert!(outcome.result.is_none(), "no verdict: {:?}", outcome.result);
    let checkpoint = checkpoint.expect("a suspension leaves a checkpoint");

    release.notify_waiters();
    let outcome = harness
        .resume(checkpoint)
        .expect("resumes")
        .execute(RunOptions::new())
        .await
        .expect("the run ends");
    assert!(
        matches!(&outcome.result, Some(Ok(_))),
        "the resumed run completes: {:?}",
        outcome.result
    );
}

#[tokio::test]
async fn a_run_streams_its_lifecycle_events() {
    use sven_vocab::{HarnessEvent, SessionEvent};

    let (engine, _p) = engine_with(vec![says("resilience")]);
    let harness = engine.load_harness_from_str(GREET).expect("loads");
    let mut run = harness
        .start(json!({"topic": "reliability"}))
        .expect("starts");
    let mut events = run.events();
    let outcome = run.execute(RunOptions::new()).await.expect("ends");
    assert_eq!(outcome.conclusion, RunConclusion::Success);

    let mut seen = Vec::new();
    loop {
        match events.try_recv() {
            Ok(SessionEvent::Harness { run_id, event }) => {
                assert_eq!(run_id, run.run_id(), "every event names its run");
                seen.push(event);
            }
            Ok(_) => continue,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    assert!(
        matches!(
            seen.first(),
            Some(HarnessEvent::Started { plan_digest, bundle_digest: None, .. })
                if plan_digest == harness.digest()
        ),
        "the stream opens with the run's start: {seen:?}"
    );
    assert!(
        seen.iter().any(|event| matches!(
            event,
            HarnessEvent::StepStarted { step, primitive }
                if step == "ask__work" && primitive == "predict"
        )),
        "the stream records each step's start: {seen:?}"
    );
    assert!(
        seen.iter().any(|event| matches!(
            event,
            HarnessEvent::StepCompleted { step, .. } if step == "ask__work"
        )),
        "the stream records each step's completion: {seen:?}"
    );
    assert!(
        matches!(seen.last(), Some(HarnessEvent::Completed { .. })),
        "the stream closes with the run's outcome: {seen:?}"
    );
}

#[tokio::test]
async fn a_cancelled_run_settles_into_its_own_cancelled_state() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, Arc::clone(&release));
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let cancel = CancelToken::new();
    let worker_cancel = cancel.clone();
    let mut run = harness.start(json!({})).expect("starts");
    let worker = tokio::spawn(async move {
        let outcome = run
            .execute(RunOptions::new().cancel(worker_cancel))
            .await
            .expect("the run ends");
        (outcome, run.checkpoint())
    });
    started_rx.await.expect("the run parks on its tool");
    cancel.cancel();
    let (outcome, checkpoint) = worker.await.expect("ends");

    assert_eq!(outcome.conclusion, RunConclusion::Cancelled);
    assert!(
        matches!(&outcome.result, Some(Err(failure)) if failure.code == "cancelled"),
        "a cancelled run reports its own cancelled failure, got {:?}",
        outcome.result
    );
    assert!(
        checkpoint.is_none(),
        "a cancelled run is terminal; there is nothing to resume"
    );
    release.notify_waiters();
}

#[tokio::test]
async fn a_checkpoint_refuses_a_changed_program() {
    let (started_tx, started_rx) = oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let engine = gated_engine(started_tx, release.clone());
    let harness = engine.load_harness_from_str(GATED).expect("loads");

    let mut run = harness.start(json!({})).expect("starts");
    let worker = tokio::spawn(async move {
        run.execute(RunOptions::new().deadline(std::time::Duration::from_millis(200)))
            .await
            .expect("the run stops");
        run.checkpoint()
    });
    started_rx.await.expect("the run parks on its tool");
    let checkpoint = worker.await.expect("ends").expect("checkpoints");

    // A different engine, a different program that only claims the same
    // name: the checkpoint must refuse it.
    let (other, _p) = engine_with(vec![]);
    let edited = other
        .load_harness_from_str(GATED_EDITED)
        .expect("the edited harness loads on its own engine");
    let error = match edited.resume(checkpoint) {
        Err(error) => error,
        Ok(_) => panic!("a changed program must not adopt the checkpoint"),
    };
    assert!(
        matches!(error, CallError::Precondition(_)),
        "the refusal is a precondition, not a crash: {error:?}"
    );
    release.notify_waiters();
}

#[tokio::test]
async fn runs_through_one_engine_are_independent() {
    let (engine, _p) = engine_with(vec![says("alpha"), says("beta")]);
    let harness = engine
        .load_harness_from_str(GREET)
        .expect("a valid harness loads");

    let mut first = harness.start(json!({"topic": "salt"})).expect("starts");
    let mut second = harness.start(json!({"topic": "pepper"})).expect("starts");
    let (first, second) = tokio::join!(
        first.execute(RunOptions::new()),
        second.execute(RunOptions::new()),
    );
    let first = first.expect("the first run ends");
    let second = second.expect("the second run ends");
    assert!(
        matches!(&first.result, Some(Ok(value)) if value["topic"] == json!("salt")),
        "{first:?}"
    );
    assert!(
        matches!(&second.result, Some(Ok(value)) if value["topic"] == json!("pepper")),
        "{second:?}"
    );
}

#[tokio::test]
async fn a_run_executes_once() {
    let (engine, _p) = engine_with(vec![says("resilience")]);
    let harness = engine.load_harness_from_str(GREET).expect("loads");
    let mut run = harness
        .start(json!({"topic": "reliability"}))
        .expect("starts");
    run.execute(RunOptions::new()).await.expect("ends");
    let error = run
        .execute(RunOptions::new())
        .await
        .expect_err("a second execute is refused");
    assert!(matches!(error, CallError::Precondition(_)), "{error:?}");
}
