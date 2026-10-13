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

use serde_json::{json, Value};
use sven_model_mock::FailingMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy as ToolApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    CancelToken, Engine, EngineBuilder, RunConclusion, RunOptions,
};

fn builder() -> EngineBuilder {
    Engine::builder().model_provider(Arc::new(FailingMockProvider::new("no model here")) as Arc<_>)
}

const VERIFYING_BRANCHES: &str = r#"
api_version: "1"
name: verifying
entry: split
capabilities: [SpawnChild, RunVerifier]
limits: { max_steps: 50, max_delegate_depth: 3, max_parallel: 4 }
steps:
  split:
    parallel: { branches: [check_a, check_b], join: merge }
  check_a:
    verify: { spec: { kind: file_exists, path: "a.txt" }, store_as: verdict, then: out_a }
  out_a:
    return: { value: "{{ steps.check_a.value }}" }
  check_b:
    verify: { spec: { kind: file_exists, path: "b.txt" }, store_as: verdict, then: out_b }
  out_b:
    return: { value: "{{ steps.check_b.value }}" }
  merge:
    return: { value: "{{ steps.split.value }}" }
"#;

#[tokio::test]
async fn parallel_verifications_each_settle_their_own_branch() {
    let root = tempfile::tempdir().expect("a tempdir");
    std::fs::write(root.path().join("a.txt"), "present").expect("seed a.txt");

    // Branches finish in either order; the join reads them in declared order.
    for round in 0..20 {
        let engine = builder()
            .project_root(root.path())
            .build()
            .expect("an engine builds");
        let harness = engine
            .load_harness_from_str(VERIFYING_BRANCHES)
            .expect("loads");
        let mut run = harness.start(json!({})).expect("starts");
        let outcome = tokio::time::timeout(Duration::from_secs(20), run.execute(RunOptions::new()))
            .await
            .expect("the run ends")
            .expect("an outcome");

        let Some(Ok(Value::Array(verdicts))) = &outcome.result else {
            panic!(
                "round {round}: the join returns both verdicts: {:?}",
                outcome.result
            );
        };
        assert_eq!(verdicts[0], json!("passed"), "round {round}");
        assert!(
            verdicts[1]
                .as_str()
                .is_some_and(|v| v.starts_with("failed")),
            "round {round}: the missing file fails its own branch: {verdicts:?}"
        );
    }
}

const ROOT: &str = r#"
api_version: "1"
name: root
entry: down
capabilities: [SpawnChild]
limits: { max_steps: 20, max_delegate_depth: 3 }
steps:
  down:
    delegate: { target: mid, store_as: got, then: done }
  done:
    return: { value: "{{ steps.down.value }}" }
"#;

const MID: &str = r#"
api_version: "1"
name: mid
entry: down
capabilities: [SpawnChild]
limits: { max_steps: 20, max_delegate_depth: 3 }
steps:
  down:
    delegate: { target: leaf, store_as: got, then: done }
  done:
    return: { value: "{{ steps.down.value }}" }
"#;

const LEAF: &str = r#"
api_version: "1"
name: leaf
entry: done
limits: { max_steps: 20 }
steps:
  done:
    return: { value: "leaf reached" }
"#;

#[tokio::test]
async fn a_delegated_child_can_delegate_within_the_depth_budget() {
    let engine = builder().build().expect("an engine builds");
    engine.load_harness_from_str(LEAF).expect("leaf loads");
    engine.load_harness_from_str(MID).expect("mid loads");
    let root = engine.load_harness_from_str(ROOT).expect("root loads");

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

const SELF_DELEGATING: &str = r#"
api_version: "1"
name: again
entry: down
capabilities: [SpawnChild]
limits: { max_steps: 20, max_delegate_depth: 3 }
steps:
  down:
    delegate: { target: again, store_as: got, then: done }
  done:
    return: { value: "{{ steps.down.value }}" }
"#;

#[tokio::test]
async fn recursive_delegation_stops_at_max_delegate_depth() {
    let engine = builder().build().expect("an engine builds");
    let again = engine
        .load_harness_from_str(SELF_DELEGATING)
        .expect("loads");

    let mut run = again.start(json!({})).expect("starts");
    let outcome = tokio::time::timeout(Duration::from_secs(10), run.execute(RunOptions::new()))
        .await
        .expect("a self-delegating harness terminates")
        .expect("an outcome");

    let rendered = format!("{:?}", outcome.result);
    assert!(
        rendered.contains("delegate_depth"),
        "the innermost run is refused for its depth, got {rendered}"
    );
}

/// A tool whose call never returns; counts how many calls started and how
/// many were dropped.
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
        "read_block"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
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

const BLOCKING_BRANCHES: &str = r#"
api_version: "1"
name: blocking
entry: split
capabilities: [SpawnChild, ReadFile]
limits: { max_steps: 50, max_delegate_depth: 3, max_parallel: 4 }
steps:
  split:
    parallel: { branches: [left, right], join: merge }
  left:
    tool: { name: read_block, then: out }
  right:
    tool: { name: read_block, then: out }
  out:
    return: { value: "never" }
  merge:
    return: { value: "{{ steps.split.value }}" }
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
        .load_harness_from_str(BLOCKING_BRANCHES)
        .expect("loads");
    let token = CancelToken::new();
    let mut run = harness.start(json!({})).expect("starts");
    let worker = {
        let token = token.clone();
        tokio::spawn(async move { run.execute(RunOptions::new().cancel(token)).await })
    };

    // Both branches are inside their blocking call before the cancel.
    for _ in 0..500 {
        if started.load(Ordering::SeqCst) == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(started.load(Ordering::SeqCst), 2, "both branches blocked");

    token.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .expect("the cancelled run ends in time")
        .expect("the worker joins")
        .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Cancelled);

    for _ in 0..500 {
        if dropped.load(Ordering::SeqCst) == 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a child's tool call kept running after the run was cancelled");
}

const BLOCKING_TOP_LEVEL: &str = r#"
api_version: "1"
name: blocking-top-level
entry: wait
capabilities: [ReadFile]
limits: { max_steps: 10 }
steps:
  wait:
    tool: { name: read_block, then: out }
  out:
    return: { value: "never" }
"#;

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
