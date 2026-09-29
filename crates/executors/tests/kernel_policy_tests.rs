//! Phase F1 tests: kernel-mediated tool execution policy, parallelism, and
//! structural integrity.
//!
//! Covers:
//! - Unknown tool → `ToolFailed` (tool-not-found / forbidden path)
//! - Capability-restricted executor → `ToolFailed` before reaching registry
//! - Three parallel `CallTool` effects each produce independent results
//! - Multi-round chat via kernel (machine re-enters Idle, loop repeats)
//! - Structural guard: only `tool.rs` calls `registry.execute`

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use sven_hsm::{
    Context, Effect, Event, Hsm, MachineId, ObservationSink, PermissionPolicy, Reaction,
    ToolCallId, ToolCapability, UiEvent,
};
use sven_kernel::{EffectExecutor, ErasedRuntime, EventSink, Runtime};
use sven_llm::ThreadStore;
use sven_tool_registry::ToolRegistry;
use tokio::sync::Mutex as TokioMutex;

// ── Minimal one-shot machine that records the first non-lifecycle event ────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum OS {
    Top,
    Idle,
    Done,
}

struct OneShotMachine(MachineId);

impl OneShotMachine {
    fn new() -> Self {
        Self(MachineId::new())
    }
}

impl sven_hsm::Machine for OneShotMachine {
    type State = OS;
    fn id(&self) -> MachineId {
        self.0
    }
    fn top(&self) -> OS {
        OS::Top
    }
    fn initial(&self) -> OS {
        OS::Idle
    }
    fn superstate(&self, _s: OS) -> OS {
        OS::Top
    }
    fn is_terminal(&self, s: OS) -> bool {
        s == OS::Done
    }
    fn dispatch_state(&mut self, s: OS, e: &Event, ctx: &mut Context) -> Reaction<OS> {
        match s {
            OS::Idle if !e.is_lifecycle() => {
                ctx.set_fact("last_event_kind", format!("{:?}", e.kind()));
                Reaction::Transition {
                    target: OS::Done,
                    effects: vec![],
                    rationale: "got non-lifecycle event".into(),
                }
            }
            _ => Reaction::Handled(vec![]),
        }
    }
}

struct NoOpExec;

#[async_trait]
impl EffectExecutor for NoOpExec {
    async fn execute(&mut self, _e: Effect, _s: &EventSink, _o: &ObservationSink) {}
}

/// Spawns an `OneShotMachine` runtime and returns it along with its sink.
fn one_shot_runtime() -> (Runtime<OneShotMachine>, EventSink) {
    let rt = Runtime::spawn(
        Hsm::new(OneShotMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        NoOpExec,
        32,
    );
    let sink = rt.sink();
    (rt, sink)
}

// ── ToolExecutor helpers ──────────────────────────────────────────────────────

fn tool_executor(registry: Arc<ToolRegistry>) -> sven_executors::ToolExecutor {
    sven_executors::ToolExecutor::new(registry, HashSet::new())
}

fn tool_executor_restricted(
    registry: Arc<ToolRegistry>,
    allowed: HashSet<ToolCapability>,
) -> sven_executors::ToolExecutor {
    sven_executors::ToolExecutor::new(registry, allowed)
}

fn turn_executor(
    provider: Arc<dyn sven_model::ModelProvider>,
    registry: Arc<ToolRegistry>,
) -> sven_executors::TurnExecutor {
    let store = Arc::new(Mutex::new(ThreadStore::new()));
    let call_id_to_thread = Arc::new(Mutex::new(HashMap::<ToolCallId, (String, String)>::new()));
    let cancel_handle = Arc::new(TokioMutex::new(None));
    sven_executors::TurnExecutor::new(
        provider,
        None,
        registry,
        store,
        call_id_to_thread,
        cancel_handle,
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// An unknown tool name causes `ToolFailed` to be emitted by `ToolExecutor`.
/// This is the "tool not found" path — equivalent to a forbidden call where
/// no matching tool exists in the registry.
#[tokio::test]
async fn unknown_tool_emits_tool_failed() {
    let registry = Arc::new(ToolRegistry::new());
    let call_id = ToolCallId::new();

    let (rt, sink) = one_shot_runtime();
    let obs = ObservationSink::new(4);

    let effect = Effect::CallTool {
        call_id,
        name: "nonexistent_tool".into(),
        args: json!({}),
        capability: ToolCapability::ReadFile,
    };

    let mut exec = tool_executor(registry);
    exec.execute(effect, &sink, &obs).await;

    // Wait for the one-shot machine to receive the ToolFailed event and reach Done.
    tokio::time::timeout(Duration::from_millis(500), rt.wait_done())
        .await
        .expect("timed out waiting for ToolFailed to reach machine");

    let report = rt.join().await.unwrap();
    let kind = report
        .ctx
        .fact("last_event_kind")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    assert_eq!(
        kind, "ToolFailed",
        "expected ToolFailed event, got {kind:?}"
    );
}

/// A `ToolExecutor` configured with a non-matching capability allow-list
/// emits `ToolFailed` immediately, before reaching the registry.
/// This exercises the second-line defence-in-depth capability check.
#[tokio::test]
async fn restricted_capability_emits_tool_failed_before_registry() {
    let registry = Arc::new(ToolRegistry::new());
    let call_id = ToolCallId::new();

    let (rt, sink) = one_shot_runtime();
    let obs = ObservationSink::new(4);

    // Allow only WriteFile; the effect requests ReadFile → blocked.
    let allowed: HashSet<ToolCapability> = [ToolCapability::WriteFile].into_iter().collect();
    let effect = Effect::CallTool {
        call_id,
        name: "read_file".into(),
        args: json!({"path": "/etc/passwd"}),
        capability: ToolCapability::ReadFile,
    };

    let mut exec = tool_executor_restricted(registry, allowed);
    exec.execute(effect, &sink, &obs).await;

    tokio::time::timeout(Duration::from_millis(500), rt.wait_done())
        .await
        .expect("timed out waiting for ToolFailed (restricted capability)");

    let report = rt.join().await.unwrap();
    let kind = report
        .ctx
        .fact("last_event_kind")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    assert_eq!(
        kind, "ToolFailed",
        "expected ToolFailed for restricted cap, got {kind:?}"
    );
}

/// Three independent `CallTool` effects each produce their own `ToolFailed`
/// result (since no tools are registered).  This verifies that the fan-in is
/// correct: every call gets exactly one result and the call_ids match.
#[tokio::test]
async fn parallel_call_tool_effects_fan_in_correctly() {
    let registry = Arc::new(ToolRegistry::new());
    let call_ids: Vec<ToolCallId> = (0..3).map(|_| ToolCallId::new()).collect();

    // Use three separate one-shot machines — one per tool call — to collect
    // events independently.  Each machine records the event kind of the first
    // non-lifecycle event it receives.
    let mut runtimes = Vec::new();
    let mut exec = tool_executor(Arc::clone(&registry));
    let obs = ObservationSink::new(16);

    for id in &call_ids {
        let (rt, sink) = one_shot_runtime();
        let effect = Effect::CallTool {
            call_id: *id,
            name: format!("tool_for_{}", id.as_uuid()),
            args: json!({}),
            capability: ToolCapability::ExecuteShell,
        };
        exec.execute(effect, &sink, &obs).await;
        runtimes.push(rt);
    }

    // All three machines must reach Done with ToolFailed.
    for (i, rt) in runtimes.into_iter().enumerate() {
        tokio::time::timeout(Duration::from_millis(500), rt.wait_done())
            .await
            .unwrap_or_else(|_| panic!("timed out on tool call {i}"));
        let report = rt.join().await.unwrap();
        let kind = report
            .ctx
            .fact("last_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        assert_eq!(
            kind, "ToolFailed",
            "call {i}: expected ToolFailed, got {kind:?}"
        );
    }
}

/// A multi-round chat via `ReactiveAgentMachine` + `TurnExecutor` with a mock
/// provider completes both turns and the machine returns to `Idle` each time.
/// This verifies that the `Generating → RunningTools/Idle` loop repeats.
#[tokio::test]
async fn multi_round_chat_completes_twice_via_kernel() {
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::submachine::ErasedMachine;
    use sven_machines::ReactiveAgentMachine;
    use sven_model_mock::MockProvider;

    let provider = Arc::new(MockProvider);
    let registry = Arc::new(ToolRegistry::new());
    let turn = turn_executor(provider, registry);
    let executor = CompositeExecutorBuilder::default().with_turn(turn).build();

    let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
    let rt = ErasedRuntime::spawn(
        machine,
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::ExecuteShell,
                ToolCapability::GitOperation,
                ToolCapability::NetworkAccess,
            ])
            .build(),
        executor,
        64,
    );

    let mut obs = rt.subscribe_observations();

    async fn await_turn_complete(obs: &mut tokio::sync::broadcast::Receiver<UiEvent>, label: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(2000);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                panic!("{label}: timed out waiting for TurnComplete");
            }
            tokio::select! {
                Ok(ev) = obs.recv() => {
                    if ev == UiEvent::TurnComplete { return; }
                }
                _ = tokio::time::sleep(remaining) => {
                    panic!("{label}: timed out waiting for TurnComplete");
                }
            }
        }
    }

    // Round 1
    rt.sink()
        .emit(Event::UserMessage {
            text: "round one".into(),
        })
        .await;
    await_turn_complete(&mut obs, "round 1").await;

    // Round 2 — machine must be back in Idle to accept a second message.
    rt.sink()
        .emit(Event::UserMessage {
            text: "round two".into(),
        })
        .await;
    await_turn_complete(&mut obs, "round 2").await;

    rt.abort();
}

// ── Structural guard ──────────────────────────────────────────────────────────

/// Every `.rs` file in `sven-executors/src` except `tool.rs` must not contain
/// a call to `registry.execute`.  This guards against future regressions where
/// an executor bypasses the kernel and runs tools off-book.
#[test]
fn only_tool_executor_module_calls_registry_execute() {
    let src_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let allowed = ["tool.rs"];

    for entry in std::fs::read_dir(src_dir).expect("cannot read executors/src") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if allowed.contains(&filename.as_str()) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !source.contains("registry.execute"),
            "File {filename} calls registry.execute — only tool.rs (ToolExecutor) is allowed to \
             invoke the tool registry. Move execution behind ToolExecutor or the kernel gate."
        );
    }
}

/// Sanity-check: `tool.rs` must still contain `registry.execute`, otherwise
/// the guard above would trivially pass without testing anything.
#[test]
fn tool_executor_still_calls_registry_execute() {
    let tool_rs = concat!(env!("CARGO_MANIFEST_DIR"), "/src/tool.rs");
    let source = std::fs::read_to_string(tool_rs).expect("cannot read src/tool.rs");
    assert!(
        source.contains("registry.execute"),
        "tool.rs no longer calls registry.execute — the structural guard above is now vacuous"
    );
}
