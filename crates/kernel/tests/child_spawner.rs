//! Kernel tests: `Effect::InstantiateSubmachine` fan-out.
//!
//! Verifies that the runtime hands `InstantiateSubmachine` effects to an
//! injected [`ChildSpawner`] which runs each child **concurrently on its own
//! task with an isolated [`Context`]**, and that each child's terminal result
//! flows back to the parent as `Event::Internal(SubmachineCompleted { result })`
//! so the parent can aggregate it. This is the substrate the SDLC
//! `DecomposeIntoTasks` fan-out rides on.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::event::InternalEvent;
use sven_hsm::{
    Context, Effect, Event, Hsm, Machine, MachineId, ObservationSink, PermissionPolicy, Reaction,
};
use sven_kernel::{ChildSpawner, EffectExecutor, EventSink, Runtime};

// ── A trivial child machine: doubles the `task` fact it was seeded with ───────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CState {
    Top,
    Run,
    Done,
}

struct ChildMachine {
    id: MachineId,
}

impl ChildMachine {
    fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl Machine for ChildMachine {
    type State = CState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> CState {
        CState::Top
    }
    fn initial(&self) -> CState {
        CState::Run
    }
    fn superstate(&self, _s: CState) -> CState {
        CState::Top
    }
    fn is_terminal(&self, s: CState) -> bool {
        s == CState::Done
    }
    fn all_states(&self) -> Vec<CState> {
        vec![CState::Run, CState::Done]
    }

    fn dispatch_state(
        &mut self,
        state: CState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<CState> {
        match state {
            CState::Top => Reaction::Ignored,
            CState::Run => match event {
                // The "work": read the isolated seed fact, double it, finish.
                Event::UserMessage { .. } => {
                    let task = ctx.fact("task").and_then(Value::as_i64).unwrap_or(0);
                    ctx.set_fact("out", task * 2);
                    Reaction::transition(CState::Done, [], "computed")
                }
                _ => Reaction::parent(CState::Top),
            },
            CState::Done => Reaction::Ignored,
        }
    }
}

// ── A spawner that runs each child as its own real `Runtime` ──────────────────

/// Spawns a fully-isolated child `Runtime` per descriptor, drives it to its
/// terminal state, then reports the harvested result up to the parent. Tracks
/// the peak number of simultaneously-live children to prove real parallelism:
/// every child waits on a shared [`tokio::sync::Barrier`] sized to the fan-out
/// while counted as live, so overlap is *forced* deterministically (the old
/// sleep-stagger only made overlap likely, which was flaky under load).
struct RealChildSpawner {
    live: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    rendezvous: Arc<tokio::sync::Barrier>,
}

#[async_trait]
impl ChildSpawner for RealChildSpawner {
    async fn spawn_child(&self, machine: MachineId, descriptor: Value, parent: EventSink) {
        let live = Arc::clone(&self.live);
        let peak = Arc::clone(&self.peak);
        let rendezvous = Arc::clone(&self.rendezvous);
        // Spawn-and-forget: spawn_child must return promptly so the parent loop
        // is never blocked and siblings start concurrently.
        tokio::spawn(async move {
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);

            // Rendezvous while live: every sibling must be live at once
            // before any proceeds — deterministic overlap, no sleeps. The
            // timeout keeps a regression (children spawned sequentially)
            // failing the peak assertion instead of hanging the test.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(10), rendezvous.wait()).await;

            // Each child gets a *fresh* context seeded only with its own task.
            let mut child_ctx = Context::new();
            child_ctx.set_fact("task", descriptor.clone());

            let rt = Runtime::spawn(
                Hsm::new(ChildMachine::new()),
                child_ctx,
                PermissionPolicy::builder().build(),
                NoopExec,
                16,
            );
            let _ = rt.post(Event::user_message("go")).await;
            rt.wait_done().await;
            let report = rt.join().await.expect("child runtime joined");
            let result = report.ctx.fact("out").cloned().unwrap_or(Value::Null);

            live.fetch_sub(1, Ordering::SeqCst);
            let _ = parent
                .emit(Event::Internal(InternalEvent::SubmachineCompleted {
                    machine: machine.as_uuid().to_string(),
                    result,
                }))
                .await;
        });
    }
}

/// Executor that does nothing (children here emit no I/O effects).
struct NoopExec;

#[async_trait]
impl EffectExecutor for NoopExec {
    async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
}

// ── The parent: fans out N children on entry, sums their results ──────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum PState {
    Top,
    FanOut,
    Done,
}

struct FanOutMachine {
    id: MachineId,
    n: i64,
}

impl FanOutMachine {
    fn new(n: i64) -> Self {
        Self {
            id: MachineId::new(),
            n,
        }
    }
}

impl Machine for FanOutMachine {
    type State = PState;

    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> PState {
        PState::Top
    }
    fn initial(&self) -> PState {
        PState::FanOut
    }
    fn superstate(&self, _s: PState) -> PState {
        PState::Top
    }
    fn is_terminal(&self, s: PState) -> bool {
        s == PState::Done
    }
    fn all_states(&self) -> Vec<PState> {
        vec![PState::FanOut, PState::Done]
    }

    fn dispatch_state(
        &mut self,
        state: PState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<PState> {
        match state {
            PState::Top => Reaction::Ignored,
            PState::FanOut => match event {
                Event::Internal(InternalEvent::Entry) => {
                    ctx.set_fact("remaining", self.n);
                    ctx.set_fact("sum", 0i64);
                    let effects: Vec<Effect> = (1..=self.n)
                        .map(|i| Effect::InstantiateSubmachine {
                            machine: MachineId::new(),
                            descriptor: json!(i),
                        })
                        .collect();
                    Reaction::effects(effects)
                }
                Event::Internal(InternalEvent::SubmachineCompleted { result, .. }) => {
                    let v = result.as_i64().unwrap_or(0);
                    let sum = ctx.fact("sum").and_then(Value::as_i64).unwrap_or(0) + v;
                    let remaining = ctx.fact("remaining").and_then(Value::as_i64).unwrap_or(0) - 1;
                    ctx.set_fact("sum", sum);
                    ctx.set_fact("remaining", remaining);
                    if remaining <= 0 {
                        Reaction::transition(PState::Done, [], "all children done")
                    } else {
                        Reaction::handled()
                    }
                }
                _ => Reaction::parent(PState::Top),
            },
            PState::Done => Reaction::Ignored,
        }
    }
}

#[tokio::test]
async fn instantiate_submachine_fans_out_children_and_aggregates_results() {
    let live = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let spawner = Arc::new(RealChildSpawner {
        live: Arc::clone(&live),
        peak: Arc::clone(&peak),
        // Sized to the fan-out below: all three children must be live at once.
        rendezvous: Arc::new(tokio::sync::Barrier::new(3)),
    });

    let rt = Runtime::spawn_with_children(
        Hsm::new(FanOutMachine::new(3)),
        Context::new(),
        PermissionPolicy::builder().build(),
        NoopExec,
        32,
        Some(spawner),
    );

    rt.wait_done().await;
    let report = rt.join().await.expect("parent runtime joined");

    // Children 1,2,3 each doubled their seed: 2 + 4 + 6 = 12.
    assert_eq!(
        report.ctx.fact("sum").and_then(Value::as_i64),
        Some(12),
        "parent must aggregate every child's result"
    );
    assert_eq!(
        report.ctx.fact("remaining").and_then(Value::as_i64),
        Some(0)
    );
    assert!(
        peak.load(Ordering::SeqCst) >= 3,
        "all three children must be live concurrently (peak live = {})",
        peak.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn no_spawner_leaves_instantiate_effects_for_the_executor() {
    // Without a ChildSpawner the InstantiateSubmachine effect falls through to
    // the executor (historical no-op), so the machine simply never completes.
    let rt = Runtime::spawn(
        Hsm::new(FanOutMachine::new(2)),
        Context::new(),
        PermissionPolicy::builder().build(),
        NoopExec,
        16,
    );

    // Give the init dispatch time to run; the machine stays in FanOut forever.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(rt.status().state_label, "FanOut");
    assert!(!rt.status().done);
    rt.abort();
}
