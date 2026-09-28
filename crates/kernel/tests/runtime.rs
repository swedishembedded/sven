//! Runtime (Active Object) tests: deterministic virtual-time timeouts, the
//! permission gate rejecting effects before execution, and the
//! executor-result-re-enters-the-queue loop.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{AgentMachine, GuardMachine, TState, TimerMachine};
use serde_json::Value;
use sven_hsm::{
    Context, Effect, Event, Hsm, ObservationSink, PermissionPolicy, ToolCapability,
};
use sven_kernel::{Clock, EffectExecutor, ErasedRuntime, EventSink, Runtime, TimerService, VirtualClock};

/// Executor that turns `ScheduleTimeout`/`CancelTimeout` into real timer tasks
/// via a [`TimerService`] backed by the injected clock.
struct TimerExec {
    clock: Arc<dyn Clock>,
    timer: Option<TimerService>,
}

#[async_trait]
impl EffectExecutor for TimerExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let timer = self
            .timer
            .get_or_insert_with(|| TimerService::new(Arc::clone(&self.clock), sink.clone()));
        match effect {
            Effect::ScheduleTimeout { timer_id, duration } => timer.schedule(timer_id, duration),
            Effect::CancelTimeout { timer_id } => timer.cancel(timer_id),
            _ => {}
        }
    }
}

#[tokio::test]
async fn virtual_time_timeout_drives_machine_to_terminal() {
    let clock = VirtualClock::new();
    let exec = TimerExec {
        clock: Arc::new(clock.clone()),
        timer: None,
    };

    let rt = Runtime::spawn(
        Hsm::new(TimerMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // Kick the machine into Waiting; its entry schedules a 30s timeout.
    assert!(rt.post(Event::user_message("start")).await);
    rt.wait_for_state("Waiting").await;
    assert_eq!(rt.status().state_label, "Waiting");
    assert!(!rt.status().done, "must not fire before time advances");

    // Advance virtual time past the deadline; the timer fires deterministically.
    clock.advance(Duration::from_secs(31));
    rt.wait_done().await;

    let report = rt.join().await.expect("runtime task joined");
    assert_eq!(report.hsm.state(), TState::Fired);
}

/// No-op executor that simply records the effects it was handed.
struct RecordingExec {
    seen: Arc<Mutex<Vec<sven_hsm::EffectKind>>>,
}

#[async_trait]
impl EffectExecutor for RecordingExec {
    async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
        self.seen.lock().unwrap().push(effect.kind());
    }
}

#[tokio::test]
async fn runtime_rejects_forbidden_effects_before_executing_them() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let exec = RecordingExec {
        seen: Arc::clone(&seen),
    };

    // Empty policy: the GuardMachine's entry CallTool(ExecuteShell) is forbidden.
    let rt = Runtime::spawn(
        Hsm::new(GuardMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // Wait until the initial dispatch has been processed and published.
    let mut status = rt.status_watch();
    let _ = status.wait_for(|s| s.last_error.is_some()).await;

    let st = rt.status();
    assert!(
        st.last_error.is_some(),
        "the forbidden effect must produce a permission error"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "no effect may execute when the batch is rejected"
    );

    // The audit trail records the rejection.
    let audit = rt.audit_snapshot();
    assert!(
        audit
            .iter()
            .any(|r| matches!(r.outcome, sven_hsm::AuditOutcome::Rejected)),
        "a Rejected audit record must be present"
    );

    rt.abort();
}

/// Executor that answers each `CallLlm` by posting a typed `LlmProposedResponse`
/// back into the queue - exercising the "results re-enter as events" loop.
struct LlmExec {
    calls: Arc<Mutex<u32>>,
}

#[async_trait]
impl EffectExecutor for LlmExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        if let Effect::CallLlm { .. } = effect {
            *self.calls.lock().unwrap() += 1;
            sink.emit(Event::LlmProposedResponse {
                text: "here you go".into(),
            })
            .await;
        }
    }
}

/// How long the spawned "tool" in [`SpawnedToolExec`] takes to produce its
/// result. Long enough that a capture served during the flight is unmistakable.
const TOOL_DELAY: Duration = Duration::from_millis(150);

/// Executor that mimics the real `ToolExecutor`'s concurrency contract:
/// `CallTool` is spawn-and-forget (the task runs concurrently and its result
/// event re-enters the queue when it finishes). Other effects are ignored.
struct SpawnedToolExec;

#[async_trait]
impl EffectExecutor for SpawnedToolExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        if let Effect::CallTool { call_id, .. } = effect {
            let sink = sink.clone();
            tokio::spawn(async move {
                tokio::time::sleep(TOOL_DELAY).await;
                sink.emit(Event::ToolSucceeded {
                    call_id,
                    observation: Value::Null,
                })
                .await;
            });
        }
    }
}

/// Minimal machine for the capture test:
///
///   Top
///    +- Idle      (UserMessage -> Busy, entry emits a CallTool)
///    +- Busy      (ToolSucceeded -> Idle)
///
/// The tool capability is `ReadFile`, which needs no approval, so the call is
/// dispatched immediately - reproducing the spawn-and-forget window without
/// approval-flow semantics.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CState {
    Top,
    Idle,
    Busy,
}

struct ToolThenWait {
    id: sven_hsm::MachineId,
}

impl Default for ToolThenWait {
    fn default() -> Self {
        Self {
            id: sven_hsm::MachineId::new(),
        }
    }
}

impl sven_hsm::Machine for ToolThenWait {
    type State = CState;

    fn id(&self) -> sven_hsm::MachineId {
        self.id
    }

    fn top(&self) -> CState {
        CState::Top
    }

    fn initial(&self) -> CState {
        CState::Idle
    }

    fn superstate(&self, _state: CState) -> CState {
        CState::Top
    }

    fn all_states(&self) -> Vec<CState> {
        vec![CState::Idle, CState::Busy]
    }

    fn dispatch_state(
        &mut self,
        state: CState,
        event: &Event,
        _ctx: &mut sven_hsm::Context,
    ) -> sven_hsm::Reaction<CState> {
        use sven_hsm::Reaction;
        match state {
            CState::Top => Reaction::Ignored,
            CState::Idle => match event {
                Event::UserMessage { .. } => Reaction::transition(
                    CState::Busy,
                    [Effect::CallTool {
                        call_id: common::fixed_tool_id(),
                        name: "reader".into(),
                        capability: ToolCapability::ReadFile,
                        args: Value::Null,
                    }],
                    "run the tool",
                ),
                _ => Reaction::parent(CState::Top),
            },
            CState::Busy => match event {
                Event::ToolSucceeded { .. } => Reaction::transition(CState::Idle, [], "settled"),
                _ => Reaction::parent(CState::Top),
            },
        }
    }
}

/// A capture must not be served while a dispatched tool call is still awaiting
/// its result event.
///
/// The capture is the engine's "nothing left to do" signal: `Agent::send`
/// races it against the observation stream and treats a served snapshot as the
/// end of the turn. With tools spawned concurrently, the event queue drains
/// *while the tools run* - so an unguarded capture hands back a mid-turn
/// snapshot, `send` returns whatever text it has collected so far, and the
/// spawned tool is torn down with the session: the turn ends with tool calls
/// pending and no result ever dispatched (observed in production as a
/// "completed" run whose reply was whitespace while a grep was in flight).
#[tokio::test]
async fn a_capture_is_not_served_while_dispatched_tools_are_in_flight() {
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(ToolThenWait::default())),
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        SpawnedToolExec,
        16,
    );

    // Kick the machine into Busy; its entry emits `CallTool`, which the
    // executor spawns and returns from - the event queue is empty while the
    // "tool" still runs for TOOL_DELAY.
    assert!(rt.post(Event::user_message("hi")).await);
    rt.wait_for_state("Busy").await;

    // Request the capture mid-flight - exactly where `Agent::send` does.
    let started = std::time::Instant::now();
    let snapshot = rt.capture().await.expect("a snapshot is served");

    // The snapshot must be taken only after the tool result was dispatched,
    // i.e. the machine has moved past the state it was in while the tool ran.
    // (Unguarded, the capture resolves immediately and snapshots "Busy".)
    assert_ne!(
        snapshot.state, "Busy",
        "capture must not snapshot the machine while its tool is in flight"
    );
    assert!(
        started.elapsed() >= TOOL_DELAY,
        "capture resolved {:?} in, before the tool result could have arrived",
        started.elapsed()
    );
    rt.abort();
}

#[tokio::test]
async fn executor_results_re_enter_the_queue_as_events() {
    let calls = Arc::new(Mutex::new(0));
    let exec = LlmExec {
        calls: Arc::clone(&calls),
    };

    let rt = Runtime::spawn(
        Hsm::new(AgentMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // UserMessage -> Drafting (emits CallLlm). The executor posts
    // LlmProposedResponse, which the kernel dispatches: Drafting -> Listening.
    assert!(rt.post(Event::user_message("hi")).await);

    // Two events get processed: the UserMessage and the re-entered response.
    let mut status = rt.status_watch();
    let _ = status.wait_for(|s| s.processed >= 2).await;

    assert_eq!(rt.status().state_label, "Listening");
    assert_eq!(*calls.lock().unwrap(), 1, "the LLM was called exactly once");

    rt.abort();
}

/// Executor holding a sentinel whose `Drop` records that the consumer task's
/// stack was actually torn down.
struct SentinelExec {
    _sentinel: DropSentinel,
}

struct DropSentinel(Arc<std::sync::atomic::AtomicBool>);

impl Drop for DropSentinel {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait]
impl EffectExecutor for SentinelExec {
    async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
}

/// Dropping a `Runtime` must tear down its consumer task.
///
/// The loop is handed an owning clone of the event sink, so `rx.recv()` never
/// returns `None`; for a machine that never reports done, nothing ends the task
/// on its own. Before `AbortOnDrop`, dropping the handle left it parked forever
/// holding the context, the machine, the executor and everything the executor
/// owns — leaked on every TUI model switch, session delete and ACP teardown.
#[tokio::test]
async fn dropping_a_runtime_aborts_its_consumer_task() {
    let torn_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rt = Runtime::spawn(
        Hsm::new(AgentMachine::default()),
        Context::new(),
        PermissionPolicy::builder().build(),
        SentinelExec {
            _sentinel: DropSentinel(Arc::clone(&torn_down)),
        },
        16,
    );

    // Drive one dispatch so the task is definitely running and parked on recv.
    assert!(rt.post(Event::user_message("hello")).await);
    tokio::task::yield_now().await;
    assert!(
        !torn_down.load(std::sync::atomic::Ordering::SeqCst),
        "executor must still be alive while the runtime handle is held"
    );

    drop(rt);

    // An abort is observed at the next scheduler pass.
    for _ in 0..100 {
        if torn_down.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("consumer task still alive after the runtime was dropped");
}

/// `join` must still get an orderly report — the abort-on-drop is disarmed.
#[tokio::test]
async fn joining_a_runtime_still_returns_its_report() {
    let torn_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rt = Runtime::spawn(
        Hsm::new(AgentMachine::default()),
        Context::new(),
        PermissionPolicy::builder().build(),
        SentinelExec {
            _sentinel: DropSentinel(torn_down),
        },
        16,
    );
    assert!(rt.post(Event::user_message("done")).await);
    rt.abort();
    assert!(
        rt.join().await.is_err(),
        "an aborted task reports a join error rather than hanging"
    );
}
