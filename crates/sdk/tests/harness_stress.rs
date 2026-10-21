// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a thousand harness runs sharing one process, one mock model and one
//! tool conclude identically - none is lost, none leaks a child, a tool call
//! or a task, and concurrency changes neither outcome nor trace.
//!
//! The load is bounded on purpose: four worker threads, at most sixteen runs
//! in flight, payloads under a hundred bytes, a mock model only. The count is
//! fixed: the claim is about a thousand runs, not about a tunable load.
//!
//! Swedish Embedded AB implements load-hardened agent runtimes for its
//! clients. If your team needs expertise in proving that concurrent agent
//! workloads stay correct and leak nothing then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sven_harness_runtime::TraceRecord;
use sven_harness_spec::HarnessBundle;
use sven_model_mock::YamlMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine, RunConclusion, RunOptions,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const RUNS: usize = 1000;
const MAX_IN_FLIGHT: usize = 16;
const RUN_DEADLINE: Duration = Duration::from_secs(10);
/// A generous ceiling for the whole test; it catches a hang or a collapse in
/// throughput, not a slow machine.
const WALL_CLOCK_CEILING: Duration = Duration::from_secs(120);

/// A schema-constrained model turn, a gate, an imported workflow and a
/// returned value: every kind of step the runtime schedules.
const PROGRAM: &str = r#"
api_version: "2"
name: stress
inputs: { kind: string }
imports: { kid: ./stress-child.yaml }
execution: { limits: { model_turns: 4, delegate_depth: 2 } }
gates:
  count: { command: [count] }
workflow:
  - name: ask
    model_only: true
    schema: { type: object, required: [label], properties: { label: { type: string } } }
    task: label this
  - name: count
    verify: [count]
  - name: kid
    use: kid
outputs:
  label: { type: string, value: "{{ tasks.ask.output.label }}" }
  kid: { type: string, value: "{{ tasks.kid.output.said }}" }
"#;

/// Imported by the program: a child run of its own.
const CHILD: &str = r#"
api_version: "2"
name: stress-child
execution: { limits: { model_turns: 2 } }
workflow:
  - name: say
    model_only: true
    task: say something
outputs:
  said: { type: string, value: "{{ tasks.say.output }}" }
"#;

const MODEL_RULES: &str = r#"
responses:
  - match_type: default
    reply: '{"label": "ok"}'
"#;

/// Counts its calls and the ones still running.
struct CountHit {
    calls: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
}

struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl Tool for CountHit {
    fn name(&self) -> &str {
        "shell_gate"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }
    fn description(&self) -> &str {
        "counts calls"
    }
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let _running = Running(Arc::clone(&self.in_flight));
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Yield so concurrent runs really interleave inside the call.
        tokio::task::yield_now().await;
        ToolOutput::ok(
            &call.id,
            json!({ "exit_code": 0, "timed_out": false, "error": null,
                    "stdout": "hit", "stderr": "" })
            .to_string(),
        )
    }
}

/// What one run concluded, reduced to what must not vary between runs. The
/// calls' random ids are dropped from the trace.
#[derive(Debug, PartialEq)]
struct Shape {
    conclusion: RunConclusion,
    result: String,
    steps: u64,
    trace: Vec<String>,
}

/// The trace without its random call ids and child machine ids.
fn comparable(trace: Vec<TraceRecord>) -> Vec<String> {
    trace
        .into_iter()
        .map(|record| match record {
            TraceRecord::StepCompleted { step, .. } => format!("completed {step}"),
            TraceRecord::ChildStarted { step, .. } => format!("child started {step}"),
            TraceRecord::ChildCompleted { step, ok, .. } => {
                format!("child completed {step} {ok}")
            }
            other => format!("{other:?}"),
        })
        .collect()
}

async fn one_run(
    provider: Arc<YamlMockProvider>,
    tool: Arc<CountHit>,
    index: usize,
) -> Result<Shape, String> {
    // The registry refuses a second harness of one name, so a run owns its
    // engine; the provider and the tool are shared.
    let engine = Engine::builder()
        .model_provider(provider as Arc<_>)
        .tool(tool as Arc<_>)
        .build()
        .map_err(|e| format!("run {index}: engine: {e}"))?;
    let package = HarnessBundle {
        document: PROGRAM.to_string(),
        imports: BTreeMap::from([("stress-child.yaml".to_string(), CHILD.to_string())]),
    };
    let harness = engine
        .load_harness_from_str(&serde_yaml::to_string(&package).map_err(|e| e.to_string())?)
        .map_err(|e| format!("run {index}: load: {e}"))?;
    // A run's scratchpad is kept; each of these thousand lives in a directory
    // that goes away with the run.
    let scratch = tempfile::tempdir().map_err(|e| format!("run {index}: scratchpad: {e}"))?;
    let mut run = harness
        .start(json!({"kind": "x"}))
        .map_err(|e| format!("run {index}: start: {e}"))?
        .with_scratchpad(scratch.path());
    let outcome = run
        .execute(RunOptions::new().deadline(RUN_DEADLINE))
        .await
        .map_err(|e| format!("run {index}: execute: {e}"))?;
    Ok(Shape {
        conclusion: outcome.conclusion,
        result: format!("{:?}", outcome.result),
        steps: outcome.steps,
        trace: comparable(outcome.trace),
    })
}

fn records(trace: &[String], kind: &str) -> usize {
    let word = match kind {
        "ChildStarted" => "child started",
        _ => "child completed",
    };
    trace.iter().filter(|r| r.starts_with(word)).count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thousand_concurrent_runs_conclude_identically() {
    let provider = Arc::new(YamlMockProvider::load(MODEL_RULES).expect("rules load"));
    let tool = Arc::new(CountHit {
        calls: Arc::new(AtomicUsize::new(0)),
        in_flight: Arc::new(AtomicUsize::new(0)),
    });
    let baseline_tasks = tokio::runtime::Handle::current()
        .metrics()
        .num_alive_tasks();

    let started = Instant::now();
    let gate = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut set = JoinSet::new();
    for index in 0..RUNS {
        // The permit is taken before the spawn, so no more than
        // MAX_IN_FLIGHT runs (and tasks) exist at once.
        let permit = Arc::clone(&gate).acquire_owned().await.expect("gate open");
        let provider = Arc::clone(&provider);
        let tool = Arc::clone(&tool);
        set.spawn(async move {
            let shape = one_run(provider, tool, index).await;
            drop(permit);
            shape
        });
    }
    let mut shapes = Vec::with_capacity(RUNS);
    while let Some(joined) = set.join_next().await {
        shapes.push(joined.expect("a run task does not panic"));
    }
    let wall = started.elapsed();
    println!("harness stress: {RUNS} runs in {wall:?} ({MAX_IN_FLIGHT} in flight, 4 workers)");

    let shapes: Vec<Shape> = shapes
        .into_iter()
        .collect::<Result<_, _>>()
        .unwrap_or_else(|e| panic!("a run failed to conclude: {e}"));
    assert_eq!(shapes.len(), RUNS, "every run concluded");

    let first = &shapes[0];
    assert_eq!(first.conclusion, RunConclusion::Success, "{first:?}");
    assert!(
        first.result.contains("ok") && first.result.contains("kid"),
        "the program returned its verdict: {first:?}"
    );
    for (index, shape) in shapes.iter().enumerate() {
        assert_eq!(shape, first, "run {index} differs from run 0");
    }

    // The imported workflow ran as a child: it started and completed.
    assert_eq!(records(&first.trace, "ChildStarted"), 1, "{first:?}");
    assert_eq!(
        records(&first.trace, "ChildCompleted"),
        1,
        "every child that started completed: {first:?}"
    );
    assert_eq!(tool.calls.load(Ordering::SeqCst), RUNS, "one call per run");
    assert_eq!(tool.in_flight.load(Ordering::SeqCst), 0, "no call leaked");

    // Tasks end shortly after their runs return; give them a bounded moment.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut alive = usize::MAX;
    while Instant::now() < deadline {
        alive = tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks();
        if alive <= baseline_tasks {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        alive <= baseline_tasks,
        "tasks back to baseline {baseline_tasks}, still {alive} alive"
    );
    assert!(
        wall < WALL_CLOCK_CEILING,
        "{RUNS} runs took {wall:?}, over the {WALL_CLOCK_CEILING:?} ceiling"
    );
}
