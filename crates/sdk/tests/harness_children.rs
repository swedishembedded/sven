// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness's children are full harness runs - a branch or delegated
//! child can verify, delegate in turn within the declared depth budget, and
//! stops when its run is cancelled.
//!
//! Swedish Embedded AB implements orchestration for programmable agent
//! runtimes for its clients. If your team needs expertise in bounded
//! fan-out, delegation and cancellation then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use serde_json::{json, Value};
use sven_harness_spec::HarnessBundle;
use sven_model_mock::{FailingMockProvider, ScriptedMockProvider};
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    CancelToken, Engine, EngineBuilder, RunConclusion, RunOptions,
};

fn builder() -> EngineBuilder {
    Engine::builder().model_provider(Arc::new(FailingMockProvider::new("no model here")) as Arc<_>)
}

/// A package: a main workflow and the workflows it imports, by path.
fn package(main: &str, imports: &[(&str, &str)]) -> String {
    let bundle = HarnessBundle {
        document: main.to_string(),
        imports: imports
            .iter()
            .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
            .collect::<BTreeMap<_, _>>(),
    };
    serde_yaml::to_string(&bundle).expect("a package serializes")
}

const ROOT: &str = r#"
api_version: "2"
name: root
imports: { down: ./mid.yaml }
execution: { limits: { model_turns: 4, delegate_depth: 3 } }
workflow:
  - name: down
    use: down
outputs:
  got: { type: string, value: "{{ tasks.down.output.got }}" }
"#;

const MID: &str = r#"
api_version: "2"
name: mid
imports: { down: ./leaf.yaml }
execution: { limits: { model_turns: 4, delegate_depth: 3 } }
workflow:
  - name: down
    use: down
outputs:
  got: { type: string, value: "{{ tasks.down.output.got }}" }
"#;

const LEAF: &str = r#"
api_version: "2"
name: leaf
execution: { limits: { model_turns: 2 } }
workflow:
  - name: reach
    model_only: true
    task: Say where you are.
outputs:
  got: { type: string, value: "{{ tasks.reach.output }}" }
"#;

fn leaf_model() -> Arc<dyn sven_model::ModelProvider> {
    Arc::new(ScriptedMockProvider::always_text("leaf reached"))
}

#[tokio::test]
async fn a_delegated_child_can_delegate_within_the_depth_budget() {
    let engine = Engine::builder()
        .model_provider(leaf_model())
        .build()
        .expect("an engine builds");
    let root = engine
        .load_harness_from_str(&package(ROOT, &[("mid.yaml", MID), ("leaf.yaml", LEAF)]))
        .expect("the package loads");

    let mut run = root.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
        .await
        .expect("the run ends")
        .expect("an outcome");

    let rendered = format!("{:?}", outcome.result);
    assert!(
        matches!(&outcome.result, Some(Ok(_))) && rendered.contains("leaf reached"),
        "the leaf's value travels up two hops, got {rendered}"
    );
}

#[tokio::test]
async fn delegation_stops_at_max_delegate_depth() {
    // Three levels deep, but the middle one allows a chain of two: the depth
    // a run sits at counts the whole chain above it.
    let tight = MID.replace("delegate_depth: 3", "delegate_depth: 2");
    let engine = Engine::builder()
        .model_provider(leaf_model())
        .build()
        .expect("an engine builds");
    let root = engine
        .load_harness_from_str(&package(ROOT, &[("mid.yaml", &tight), ("leaf.yaml", LEAF)]))
        .expect("the package loads");

    let mut run = root.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.execute(RunOptions::new()))
        .await
        .expect("a too deeply delegating harness terminates")
        .expect("an outcome");

    let rendered = format!("{:?}", outcome.result);
    assert!(
        rendered.contains("delegate_depth"),
        "the innermost run is refused for its depth, got {rendered}"
    );
}

/// The gate tool a workflow's gate calls, scripted never to return; counts
/// how many calls started and how many were dropped.
struct Blocks {
    started: Arc<AtomicUsize>,
    dropped: Arc<AtomicUsize>,
}

struct DropGuard(Arc<AtomicUsize>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Tool for Blocks {
    fn name(&self) -> &str {
        "shell_gate"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    fn description(&self) -> &str {
        "never returns"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ToolApprovalPolicy {
        ToolApprovalPolicy::Auto
    }
    async fn execute(&self, _call: &ToolCall) -> ToolOutput {
        let _guard = DropGuard(Arc::clone(&self.dropped));
        self.started.fetch_add(1, Ordering::SeqCst);
        std::future::pending::<()>().await;
        unreachable!("the call is only ever dropped")
    }
}

/// A workflow whose only work is a gate that never answers.
const BLOCKER: &str = r#"
api_version: "2"
name: blocker
execution: { limits: { model_turns: 2 } }
gates:
  wait: { command: [wait-forever], timeout: 600 }
workflow:
  - name: wait
    verify: [wait]
"#;

const BLOCKING_PARENT: &str = r#"
api_version: "2"
name: parent
imports: { child: ./blocker.yaml }
execution: { limits: { model_turns: 4, delegate_depth: 3 } }
workflow:
  - name: child
    use: child
"#;

#[tokio::test]
async fn cancelling_a_run_stops_its_children() {
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let engine = builder()
        .tool(Arc::new(Blocks {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        }))
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(&package(BLOCKING_PARENT, &[("blocker.yaml", BLOCKER)]))
        .expect("loads");
    let token = CancelToken::new();
    let mut run = harness.start(json!({})).expect("starts");
    let worker = {
        let token = token.clone();
        tokio::spawn(async move { run.execute(RunOptions::new().cancel(token)).await })
    };

    // The child is inside its blocking call before the cancel.
    for _ in 0..500 {
        if started.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 1, "the child is blocked");

    token.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the cancelled run ends in time")
        .expect("the worker joins")
        .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Cancelled);

    for _ in 0..500 {
        if dropped.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a child's tool call kept running after the run was cancelled");
}

#[tokio::test]
async fn a_run_waiting_on_a_slow_child_is_stopped_by_its_bound_not_reported_stalled() {
    // The parent machine has nothing queued while its child runs, but it
    // is not stuck: the deadline, not a stall verdict, ends it.
    let engine = builder()
        .tool(Arc::new(Blocks {
            started: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicUsize::new(0)),
        }))
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(&package(BLOCKING_PARENT, &[("blocker.yaml", BLOCKER)]))
        .expect("loads");
    let mut run = harness.start(json!({})).expect("starts");

    let outcome = run
        .execute(RunOptions::new().deadline(Duration::from_millis(500)))
        .await
        .expect("an outcome");

    assert_eq!(outcome.conclusion, RunConclusion::Timeout);
}

const BLOCKING_TOP_LEVEL: &str = BLOCKER;

/// Runs the top-level blocking harness under `options`, waits for its tool
/// call to be in flight, optionally fires `cancel`, and returns once the
/// run ended; the caller then checks the call was dropped.
async fn end_a_blocked_run(
    options: impl FnOnce(&CancelToken) -> RunOptions,
    cancel_when_blocked: bool,
) -> (RunConclusion, Arc<AtomicUsize>) {
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let engine = builder()
        .tool(Arc::new(Blocks {
            started: Arc::clone(&started),
            dropped: Arc::clone(&dropped),
        }))
        .build()
        .expect("an engine builds");
    let harness = engine
        .load_harness_from_str(BLOCKING_TOP_LEVEL)
        .expect("loads");
    let token = CancelToken::new();
    let bounds = options(&token);
    let mut run = harness.start(json!({})).expect("starts");
    let worker = tokio::spawn(async move { run.execute(bounds).await });

    for _ in 0..500 {
        if started.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "the tool call is in flight"
    );
    if cancel_when_blocked {
        token.cancel();
    }
    let outcome = tokio::time::timeout(Duration::from_secs(10), worker)
        .await
        .expect("the bounded run ends in time")
        .expect("the worker joins")
        .expect("an outcome");
    (outcome.conclusion, dropped)
}

async fn assert_call_dropped(dropped: &AtomicUsize) {
    for _ in 0..300 {
        if dropped.load(Ordering::SeqCst) == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the run's own tool call kept running after the run ended");
}

#[tokio::test]
async fn cancelling_a_run_stops_its_own_tool_call() {
    let (conclusion, dropped) =
        end_a_blocked_run(|t| RunOptions::new().cancel(t.clone()), true).await;
    assert_eq!(conclusion, RunConclusion::Cancelled);
    assert_call_dropped(&dropped).await;
}

#[tokio::test]
async fn a_deadline_stops_the_tool_call_it_suspended() {
    let (conclusion, dropped) = end_a_blocked_run(
        |_| RunOptions::new().deadline(Duration::from_millis(300)),
        false,
    )
    .await;
    assert_eq!(conclusion, RunConclusion::Timeout);
    assert_call_dropped(&dropped).await;
}
