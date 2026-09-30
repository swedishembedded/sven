// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Kernel tests: the contract every child run is started under.
//!
//! A child inherits the capabilities its parent holds in the spawning state
//! and can never widen them, it is stopped when its parent is cancelled or
//! shut down, and it is stopped at its deadline.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_hsm::event::InternalEvent;
use sven_hsm::submachine::ErasedMachine;
use sven_hsm::{
    Context, Effect, Event, Hsm, Machine, MachineId, ObservationSink, PermissionPolicy, Reaction,
    RuntimeStatus, ToolCallId, ToolCapability,
};
use sven_kernel::{
    ChildRun, ChildSpawner, EffectExecutor, ErasedRuntime, EventSink, RUN_CANCELLED,
};
use tokio::sync::watch;

// ── The child: tries a shell command, or waits forever ───────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum CState {
    Top,
    Run,
    Done,
}

/// With `try_shell`, calls a shell tool on entry and records what became of
/// it; without, waits for an event that never comes.
struct Child {
    id: MachineId,
    try_shell: bool,
}

impl Machine for Child {
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
        let finish = |ctx: &mut Context, outcome: &str| {
            ctx.set_fact("out", outcome);
            Reaction::transition(CState::Done, [], "recorded")
        };
        match (state, event) {
            (CState::Run, Event::Internal(InternalEvent::Entry)) if self.try_shell => {
                Reaction::effects(vec![Effect::CallTool {
                    call_id: ToolCallId::new(),
                    name: "shell".into(),
                    capability: ToolCapability::ExecuteShell,
                    args: Value::Null,
                }])
            }
            (CState::Run, Event::ToolSucceeded { .. }) => finish(ctx, "ran"),
            (CState::Run, Event::ToolFailed { error, .. })
                if error.contains("permission denied") =>
            {
                finish(ctx, "forbidden")
            }
            (CState::Run, Event::ToolApprovalRequired { .. }) => finish(ctx, "needs approval"),
            (CState::Top | CState::Done, _) => Reaction::Ignored,
            _ => Reaction::parent(CState::Top),
        }
    }
}

/// Answers every tool call it is handed as a success.
struct ToolsSucceed;

#[async_trait]
impl EffectExecutor for ToolsSucceed {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        if let Effect::CallTool { call_id, .. } = effect {
            let _ = sink
                .emit(Event::ToolSucceeded {
                    call_id,
                    observation: json!("ok"),
                })
                .await;
        }
    }
}

// ── The spawner: runs each child as a real child run ─────────────────────────

/// What the test observes about the one child it spawned.
#[derive(Default)]
struct Observed {
    contract: Option<sven_hsm::ChildRunContract>,
    status: Option<watch::Receiver<RuntimeStatus>>,
}

struct Spawner {
    try_shell: bool,
    /// Further terms the spawner adds, as a real spawner would.
    deadline_after: Option<Duration>,
    observed: Arc<Mutex<Observed>>,
}

#[async_trait]
impl ChildSpawner for Spawner {
    async fn spawn_child(&self, machine: MachineId, _d: Value, run: ChildRun, parent: EventSink) {
        let mut run = run;
        if let Some(after) = self.deadline_after {
            let terms = sven_hsm::ChildRunContract::new(run.contract.policy.clone())
                .with_deadline(Instant::now() + after);
            run.contract = run.contract.narrow(&terms);
        }
        let child = Child {
            id: MachineId::new(),
            try_shell: self.try_shell,
        };
        let observed_contract = run.contract.clone();
        let rt = ErasedRuntime::spawn_child_run(
            Box::new(Hsm::new(child)) as Box<dyn ErasedMachine>,
            Context::new(),
            ToolsSucceed,
            16,
            None,
            run,
        );
        {
            let mut observed = self.observed.lock().unwrap();
            observed.contract = Some(observed_contract);
            observed.status = Some(rt.status_watch());
        }
        tokio::spawn(async move {
            rt.wait_done().await;
            let result = match rt.join().await {
                Ok(report) => report.ctx.fact("out").cloned().unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            let _ = parent
                .emit(Event::Internal(InternalEvent::SubmachineCompleted {
                    machine: machine.as_uuid().to_string(),
                    result,
                }))
                .await;
        });
    }
}

// ── The parent: spawns one child on entry and records its result ─────────────

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum PState {
    Top,
    Delegating,
    Done,
}

struct Parent {
    id: MachineId,
}

impl Machine for Parent {
    type State = PState;
    fn id(&self) -> MachineId {
        self.id
    }
    fn top(&self) -> PState {
        PState::Top
    }
    fn initial(&self) -> PState {
        PState::Delegating
    }
    fn superstate(&self, _s: PState) -> PState {
        PState::Top
    }
    fn is_terminal(&self, s: PState) -> bool {
        s == PState::Done
    }
    fn all_states(&self) -> Vec<PState> {
        vec![PState::Delegating, PState::Done]
    }
    fn dispatch_state(
        &mut self,
        state: PState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<PState> {
        match (state, event) {
            (PState::Delegating, Event::Internal(InternalEvent::Entry)) => {
                Reaction::effects(vec![Effect::InstantiateSubmachine {
                    machine: MachineId::new(),
                    descriptor: Value::Null,
                }])
            }
            (
                PState::Delegating,
                Event::Internal(InternalEvent::SubmachineCompleted { result, .. }),
            ) => {
                ctx.set_fact("child", result.clone());
                Reaction::transition(PState::Done, [], "child done")
            }
            (PState::Top | PState::Done, _) => Reaction::Ignored,
            _ => Reaction::parent(PState::Top),
        }
    }
}

fn parent_policy(caps: impl IntoIterator<Item = ToolCapability>) -> PermissionPolicy {
    PermissionPolicy::builder()
        .allow_in(
            PState::Delegating,
            caps.into_iter().chain([ToolCapability::SpawnChild]),
        )
        .build()
}

fn start_parent(policy: PermissionPolicy, spawner: Spawner) -> ErasedRuntime {
    ErasedRuntime::spawn_with_children(
        Box::new(Hsm::new(Parent {
            id: MachineId::new(),
        })) as Box<dyn ErasedMachine>,
        Context::new(),
        policy,
        ToolsSucceed,
        16,
        Some(Arc::new(spawner)),
    )
}

async fn child_status(observed: &Arc<Mutex<Observed>>) -> watch::Receiver<RuntimeStatus> {
    for _ in 0..500 {
        if let Some(status) = observed.lock().unwrap().status.clone() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the child was never spawned");
}

async fn child_result(parent: ErasedRuntime) -> Value {
    tokio::time::timeout(Duration::from_secs(10), parent.wait_done())
        .await
        .expect("the parent finishes");
    let report = parent.join().await.expect("parent joined");
    report.ctx.fact("child").cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_parent_without_shell_spawns_a_child_that_cannot_shell() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let parent = start_parent(
        parent_policy([ToolCapability::ReadFile]),
        Spawner {
            try_shell: true,
            deadline_after: None,
            observed: Arc::clone(&observed),
        },
    );
    assert_eq!(child_result(parent).await, json!("forbidden"));
    let contract = observed.lock().unwrap().contract.clone().unwrap();
    assert!(contract
        .policy
        .allows_in_every_state(ToolCapability::ReadFile));
    assert!(!contract
        .policy
        .allows_in_every_state(ToolCapability::ExecuteShell));

    // The control: a parent that holds shell hands it down, still subject to
    // approval because shell is inherently dangerous.
    let parent = start_parent(
        parent_policy([ToolCapability::ReadFile, ToolCapability::ExecuteShell]),
        Spawner {
            try_shell: true,
            deadline_after: None,
            observed: Arc::new(Mutex::new(Observed::default())),
        },
    );
    assert_eq!(child_result(parent).await, json!("needs approval"));
}

#[tokio::test]
async fn a_state_without_spawn_permission_cannot_start_a_child() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let parent = ErasedRuntime::spawn_with_children(
        Box::new(Hsm::new(Parent {
            id: MachineId::new(),
        })) as Box<dyn ErasedMachine>,
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        ToolsSucceed,
        16,
        Some(Arc::new(Spawner {
            try_shell: false,
            deadline_after: None,
            observed: Arc::clone(&observed),
        })),
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        observed.lock().unwrap().status.is_none(),
        "no child was started"
    );
    let rejected = parent
        .audit_snapshot()
        .iter()
        .any(|r| format!("{r:?}").contains("SpawnChild"));
    assert!(rejected, "the refusal is audited");
}

async fn wait_cancelled(mut status: watch::Receiver<RuntimeStatus>) -> RuntimeStatus {
    tokio::time::timeout(Duration::from_secs(10), status.wait_for(|s| s.done))
        .await
        .expect("the child stops")
        .expect("status channel open")
        .clone()
}

#[tokio::test]
async fn cancelling_the_parent_cancels_a_running_child() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let parent = start_parent(
        parent_policy([]),
        Spawner {
            try_shell: false,
            deadline_after: None,
            observed: Arc::clone(&observed),
        },
    );
    let status = child_status(&observed).await;
    assert!(!status.borrow().done, "the child is running");

    parent.cancel();
    let stopped = wait_cancelled(status).await;
    assert_eq!(stopped.last_error.as_deref(), Some(RUN_CANCELLED));
    assert!(parent.status().done);
}

#[tokio::test]
async fn shutting_the_parent_down_cancels_a_running_child() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let parent = start_parent(
        parent_policy([]),
        Spawner {
            try_shell: false,
            deadline_after: None,
            observed: Arc::clone(&observed),
        },
    );
    let status = child_status(&observed).await;
    drop(parent);
    let stopped = wait_cancelled(status).await;
    assert_eq!(stopped.last_error.as_deref(), Some(RUN_CANCELLED));
}

#[tokio::test]
async fn a_child_is_stopped_at_its_deadline() {
    let observed = Arc::new(Mutex::new(Observed::default()));
    let parent = start_parent(
        parent_policy([]),
        Spawner {
            try_shell: false,
            deadline_after: Some(Duration::from_millis(50)),
            observed: Arc::clone(&observed),
        },
    );
    let status = child_status(&observed).await;
    let stopped = wait_cancelled(status).await;
    assert_eq!(stopped.last_error.as_deref(), Some(RUN_CANCELLED));
    // The parent is not cancelled by its child's deadline: it hears the child
    // end and carries on.
    assert_eq!(child_result(parent).await, Value::Null);
}
