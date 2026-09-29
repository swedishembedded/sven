//! The tokio Active Object runtime that drives a [`sven_hsm::Machine`].
//!
//! Per `04-active-objects.md` §7.5, the kernel runs as a **single consumer
//! task** draining an `mpsc` event queue. That single task is what guarantees
//! *run-to-completion*: a machine is never dispatched from two tasks at once, so
//! handlers never interleave. The loop is:
//!
//! ```text
//! recv event -> machine.dispatch -> validate_effects_are_allowed
//!            -> hand each allowed effect to the EffectExecutor
//!            -> executor's results re-enter the queue as events
//! ```
//!
//! Side effects (LLM calls, tools, timers, ...) are performed by a user-supplied
//! [`EffectExecutor`] on its own tasks; results are posted back through the
//! [`EventSink`]. Timers are deterministic in tests via the [`Clock`]
//! abstraction: a [`SystemClock`] for production and a [`VirtualClock`] whose
//! time only advances when the test calls [`VirtualClock::advance`].
//!
//! Split out of `sven-hsm` (Phase 4.4 of the crate-architecture refactor
//! plan) so the kernel's pure vocabulary (`Machine`, `Effect`, `Event`,
//! `Context`, `PermissionPolicy`, `AuditRecord`) has no tokio dependency of
//! its own — only the active-object execution engine that drives it does.
//! `sven-hsm`'s outward observation plane (`ObservationSink`, a broadcast
//! channel) stays in `sven-hsm`: it's part of the kernel's event vocabulary,
//! not the execution engine, and moving it too would force a `sven-kernel`
//! dependency onto every one of `UiEvent`'s many consumers for no benefit.

mod abort_on_drop;
use abort_on_drop::AbortOnDrop;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, watch};

use serde_json::Value;

use sven_hsm::{
    classify, validate_effects_are_allowed, AuditRecord, AuditTrailHandle, Context, Effect,
    EffectDisposition, ErasedMachine, ErasedReport, Event, EventKind, Hsm, InternalEvent, Machine,
    MachineId, ObservationSink, PermissionPolicy, RuntimeReport, RuntimeStatus, Snapshot,
    StateLabel, ToolAuditRecord, ToolCallId, UiEvent,
};

mod clock;
pub use clock::{Clock, SystemClock, TimerService, VirtualClock};
mod effect_failure;
pub use effect_failure::failure_event;

/// Default capacity of the per-runtime observation broadcast channel.
const OBSERVATION_CAPACITY: usize = 1024;

/// A clonable handle used by executors and timers to post events back into the
/// kernel queue.
#[derive(Clone)]
pub struct EventSink {
    tx: mpsc::Sender<Event>,
}

impl EventSink {
    /// Posts an event, awaiting queue capacity. Returns `false` if the kernel
    /// has shut down (receiver dropped).
    pub async fn emit(&self, event: Event) -> bool {
        self.tx.send(event).await.is_ok()
    }

    /// Posts an event without awaiting; returns `false` if the queue is full or
    /// the kernel has shut down.
    pub fn try_emit(&self, event: Event) -> bool {
        self.tx.try_send(event).is_ok()
    }
}

/// Performs the I/O for effects. Implementations live in higher crates (LLM,
/// tools, UI, ...); each consumes an [`Effect`] and posts result
/// [`Event`]s back through the [`EventSink`].
#[async_trait]
pub trait EffectExecutor: Send {
    /// Performs `effect`.
    ///
    /// May post zero or more result [`Event`]s back through `sink` (the inward
    /// plane) and emit zero or more [`UiEvent`]s through `obs` (the outward
    /// streaming plane). Streaming output (token deltas, tool progress, usage)
    /// goes to `obs`; exactly the completion fact that advances the machine
    /// goes to `sink`.
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink);
}

#[async_trait]
impl EffectExecutor for Box<dyn EffectExecutor> {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        self.as_mut().execute(effect, sink, obs).await;
    }
}

/// Spawns child submachines in response to [`Effect::InstantiateSubmachine`].
///
/// The kernel itself is machine-agnostic, so it cannot build a concrete child
/// from an opaque descriptor. A `ChildSpawner` bridges that gap: given the
/// parent-assigned [`MachineId`], the descriptor, and a clone of the parent's
/// [`EventSink`], it must run the child **concurrently on its own task with an
/// isolated [`Context`]** and, when the child reaches a terminal state, post
/// `Event::Internal(InternalEvent::SubmachineCompleted { machine, result })`
/// back to the parent so the parent can aggregate the result (append-only).
///
/// `spawn_child` should return promptly (spawn-and-forget); long-running child
/// work belongs on the task it spawns, never inline, so the parent's single
/// consumer loop is never blocked. This is what makes fan-out *parallel*.
#[async_trait]
pub trait ChildSpawner: Send + Sync {
    /// Builds and starts the child identified by `machine`.
    ///
    /// Implementations post the terminal [`InternalEvent::SubmachineCompleted`]
    /// (carrying the child's result payload) into `parent` when done.
    async fn spawn_child(&self, machine: MachineId, descriptor: Value, parent: EventSink);
}

/// The child submachines in flight for a parent loop, and the spawner that
/// starts them.
///
/// The parent machine drives aggregation (it owns the append-only thread), so
/// the kernel only needs a lightweight liveness map: insert on spawn, remove on
/// [`InternalEvent::SubmachineCompleted`]. Keeping it here gives the runtime an
/// authoritative concurrent-child count for observability and shutdown.
struct Children {
    spawner: Option<Arc<dyn ChildSpawner>>,
    live: HashMap<MachineId, ()>,
}

impl Children {
    fn new(spawner: Option<Arc<dyn ChildSpawner>>) -> Self {
        Self {
            spawner,
            live: HashMap::new(),
        }
    }

    /// Runs one effect the permission gate already allowed: an
    /// [`Effect::InstantiateSubmachine`] goes to the spawner when one is
    /// configured, everything else to `executor` (which answers an
    /// instantiate it cannot serve with a failure event).
    async fn execute<E: EffectExecutor>(
        &mut self,
        effect: Effect,
        executor: &mut E,
        sink: &EventSink,
        obs: &ObservationSink,
    ) {
        match (effect, &self.spawner) {
            (
                Effect::InstantiateSubmachine {
                    machine,
                    descriptor,
                },
                Some(spawner),
            ) => {
                self.live.insert(machine, ());
                spawner.spawn_child(machine, descriptor, sink.clone()).await;
            }
            (effect, _) => executor.execute(effect, sink, obs).await,
        }
    }
}

/// Drops a completed child from the registry so the parent's concurrent-child
/// count stays accurate.
fn note_child_completion(children: &mut Children, event: &Event) {
    if let Event::Internal(InternalEvent::SubmachineCompleted { machine, .. }) = event {
        if let Ok(uuid) = uuid::Uuid::parse_str(machine) {
            children.live.remove(&MachineId::from_uuid(uuid));
        }
    }
}

/// A handle to a running kernel. Post events, observe state, and join for the
/// final report.
pub struct Runtime<M: Machine> {
    sink: EventSink,
    obs: ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    handle: AbortOnDrop<RuntimeReport<M>>,
}

impl<M> Runtime<M>
where
    M: Machine + Send + 'static,
    M::State: Send,
{
    /// Spawns the single consumer task for `hsm`.
    ///
    /// The machine is initialized inside the task (its initial-entry effects are
    /// validated and executed like any others). `queue_depth` bounds the mpsc
    /// queue.
    pub fn spawn<E>(
        hsm: Hsm<M>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        Self::spawn_with_children(hsm, ctx, policy, executor, queue_depth, None)
    }

    /// Like [`spawn`](Self::spawn) but with an optional [`ChildSpawner`] that
    /// services [`Effect::InstantiateSubmachine`] by running children
    /// concurrently. Pass `None` for the historical (no submachine) behavior.
    pub fn spawn_with_children<E>(
        hsm: Hsm<M>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
        child_spawner: Option<Arc<dyn ChildSpawner>>,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        let (tx, rx) = mpsc::channel::<Event>(queue_depth.max(1));
        let (status_tx, status_rx) = watch::channel(RuntimeStatus::default());
        let audit = Arc::new(Mutex::new(Vec::new()));
        let sink = EventSink { tx };
        let obs = ObservationSink::new(OBSERVATION_CAPACITY);

        let handle = tokio::spawn(consumer_loop(
            hsm,
            ctx,
            policy,
            executor,
            rx,
            sink.clone(),
            obs.clone(),
            status_tx,
            Arc::clone(&audit),
            child_spawner,
        ));

        Self {
            sink,
            obs,
            status_rx,
            audit,
            handle: AbortOnDrop::new(handle),
        }
    }

    /// A sink for posting events into this runtime (also usable by executors).
    #[must_use]
    pub fn sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// A clone of this runtime's outward observation sink.
    #[must_use]
    pub fn observations(&self) -> ObservationSink {
        self.obs.clone()
    }

    /// Subscribes a new receiver to this runtime's outward observation stream.
    #[must_use]
    pub fn subscribe_observations(&self) -> tokio::sync::broadcast::Receiver<UiEvent> {
        self.obs.subscribe()
    }

    /// Posts an event, awaiting queue capacity.
    pub async fn post(&self, event: Event) -> bool {
        self.sink.emit(event).await
    }

    /// The latest published status snapshot.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.status_rx.borrow().clone()
    }

    /// A fresh receiver for status updates.
    #[must_use]
    pub fn status_watch(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_rx.clone()
    }

    /// A snapshot of the audit trail accumulated so far.
    #[must_use]
    pub fn audit_snapshot(&self) -> Vec<AuditRecord> {
        self.audit.lock().expect("audit mutex poisoned").clone()
    }

    /// Waits until the machine's state label equals `label`.
    pub async fn wait_for_state(&self, label: &str) {
        let mut rx = self.status_rx.clone();
        let _ = rx
            .wait_for(|s| s.state_label == label || s.done)
            .await
            .map(|_| ());
    }

    /// Waits until the machine reaches a terminal state.
    pub async fn wait_done(&self) {
        let mut rx = self.status_rx.clone();
        let _ = rx.wait_for(|s| s.done).await.map(|_| ());
    }

    /// Aborts the consumer task without waiting (best-effort shutdown).
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Detaches the consumer task so it outlives this handle. See
    /// [`ErasedRuntime::detach`].
    pub fn detach(self) {
        // Dropping a `JoinHandle` detaches its task, which is the point.
        std::mem::drop(self.handle.disarm());
    }

    /// Awaits the consumer task and returns the final machine + context. The
    /// task ends when the machine reaches a terminal state or the queue closes.
    ///
    /// # Errors
    ///
    /// Returns the join error if the task panicked or was aborted.
    pub async fn join(self) -> std::result::Result<RuntimeReport<M>, tokio::task::JoinError> {
        self.handle.disarm().await
    }
}

#[allow(clippy::too_many_arguments)]
async fn consumer_loop<M, E>(
    mut hsm: Hsm<M>,
    mut ctx: Context,
    policy: PermissionPolicy,
    mut executor: E,
    mut rx: mpsc::Receiver<Event>,
    sink: EventSink,
    obs: ObservationSink,
    status_tx: watch::Sender<RuntimeStatus>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    child_spawner: Option<Arc<dyn ChildSpawner>>,
) -> RuntimeReport<M>
where
    M: Machine + Send + 'static,
    M::State: Send,
    E: EffectExecutor + 'static,
{
    let mut processed: u64 = 0;
    let mut last_error: Option<String> = None;
    let mut children = Children::new(child_spawner);
    // Same in-flight registry as the erased loop. The typed runtime exposes no
    // quiescence-based capture, so the set is only fed; keeping the bookkeeping
    // identical means run_effects has one contract.
    let mut inflight_tools: HashSet<ToolCallId> = HashSet::new();

    // Initial transitions run inside the single consumer task, so their entry
    // effects go through the same validate-then-execute path as everything else.
    let init_effects = hsm.init(&mut ctx);
    run_effects(
        &policy,
        hsm.state(),
        hsm.state_label(),
        &mut ctx,
        &mut executor,
        &sink,
        &obs,
        EventKind::Init,
        init_effects,
        &mut last_error,
        &mut inflight_tools,
        &mut children,
    )
    .await;

    publish(
        &status_tx,
        &audit,
        &ctx,
        &hsm,
        processed,
        last_error.clone(),
    );

    if hsm.is_done() {
        return RuntimeReport { hsm, ctx };
    }

    while let Some(event) = rx.recv().await {
        note_child_completion(&mut children, &event);
        let outcome = hsm.dispatch(&event, &mut ctx);
        let event_kind = outcome.event;
        // Emit the transition trace on the outward plane after every dispatch.
        obs.emit(UiEvent::Transition {
            from: outcome.from.clone(),
            to: outcome.to.clone(),
            event: format!("{:?}", event_kind),
        });
        let effects = outcome.effects;
        run_effects(
            &policy,
            hsm.state(),
            hsm.state_label(),
            &mut ctx,
            &mut executor,
            &sink,
            &obs,
            event_kind,
            effects,
            &mut last_error,
            &mut inflight_tools,
            &mut children,
        )
        .await;

        processed += 1;
        publish(
            &status_tx,
            &audit,
            &ctx,
            &hsm,
            processed,
            last_error.clone(),
        );

        if hsm.is_done() {
            break;
        }
    }

    RuntimeReport { hsm, ctx }
}

/// Validates a batch of effects against the policy (for the machine's current
/// state) and executes them.
///
/// * `CallTool` effects are classified **per-call**:
///   - `Allowed` → dispatched to executor immediately (spawn-like; the executor
///     is responsible for its own concurrency). The call id is recorded in
///     `inflight` until its result event re-enters the queue: with tool
///     execution spawned concurrently, an empty event queue is *not* "nothing
///     left to do", and quiescence-based captures must wait for the result.
///   - `Forbidden` → `Event::ToolFailed` is emitted directly into the sink so
///     the machine sees the denial as a normal tool result (graceful, no abort).
///     Denied calls never enter `inflight`: their result is already queued.
///   - `NeedsApproval` → `Event::ToolApprovalRequired` is emitted; the machine
///     handles the approval flow (`RequestHumanApproval` → `HumanApproved` →
///     re-emit `CallTool`). Unapproved calls never enter `inflight`: the
///     machine is genuinely waiting for outside input.
/// * All other effects are validated all-or-nothing (batch reject): a refused
///   batch is audited and each effect in it that a machine may be waiting on
///   is answered with its [`failure_event`].
#[allow(clippy::too_many_arguments)]
async fn run_effects<S, E>(
    policy: &PermissionPolicy,
    state: S,
    state_label: String,
    ctx: &mut Context,
    executor: &mut E,
    sink: &EventSink,
    obs: &ObservationSink,
    event: EventKind,
    effects: Vec<Effect>,
    last_error: &mut Option<String>,
    inflight: &mut HashSet<ToolCallId>,
    children: &mut Children,
) where
    S: std::fmt::Debug + Send,
    E: EffectExecutor,
{
    if effects.is_empty() {
        return;
    }

    // Partition effects: CallTool are handled per-call; the rest go through
    // the all-or-nothing batch check (unchanged behaviour).
    let mut tool_effects: Vec<Effect> = Vec::new();
    let mut other_effects: Vec<Effect> = Vec::new();
    for eff in effects {
        if matches!(eff, Effect::CallTool { .. }) {
            tool_effects.push(eff);
        } else {
            other_effects.push(eff);
        }
    }

    // ── Non-tool effects: all-or-nothing batch check (unchanged) ──────────────
    if !other_effects.is_empty() {
        match validate_effects_are_allowed(policy, &state, &other_effects, ctx) {
            Ok(()) => {
                *last_error = None;
                for effect in other_effects {
                    children.execute(effect, executor, sink, obs).await;
                }
            }
            Err(err) => {
                let record =
                    AuditRecord::rejected(&state_label, event, &other_effects, err.to_string());
                ctx.push_audit(record);
                *last_error = Some(err.to_string());
                obs.emit(UiEvent::Error(err.to_string()));
                // A refused effect is answered, never dropped silently: the
                // machine may be waiting on it (formal/tla/EffectDelivery.tla).
                let reason = format!("permission denied: {err}");
                for effect in &other_effects {
                    if let Some(answer) = failure_event(effect, &reason) {
                        let _ = sink.emit(answer).await;
                    }
                }
            }
        }
    }

    // ── Tool effects: per-call graceful gating ─────────────────────────────────
    for effect in tool_effects {
        let Effect::CallTool {
            ref call_id,
            ref name,
            capability,
            ..
        } = effect
        else {
            unreachable!("filtered above");
        };
        match classify(policy, &state, ctx, &effect) {
            EffectDisposition::Allowed => {
                ctx.push_tool_audit(ToolAuditRecord::started(
                    &state_label,
                    *call_id,
                    name,
                    capability,
                ));
                *last_error = None;
                // Register before execution so the consumer loop holds the
                // quiescence signal until the result event re-enters the
                // queue, no matter how the executor runs the tool.
                inflight.insert(*call_id);
                executor.execute(effect, sink, obs).await;
            }
            EffectDisposition::Forbidden(reason) => {
                ctx.push_tool_audit(ToolAuditRecord::denied(
                    &state_label,
                    *call_id,
                    name,
                    capability,
                    &reason,
                ));
                // Also push to the main audit trail so callers using
                // `audit_snapshot()` can detect the rejection.
                ctx.push_audit(AuditRecord::rejected(
                    &state_label,
                    event,
                    std::slice::from_ref(&effect),
                    reason.clone(),
                ));
                *last_error = Some(reason.clone());
                let _ = sink
                    .emit(Event::ToolFailed {
                        call_id: *call_id,
                        error: format!("permission denied: {reason}"),
                    })
                    .await;
            }
            EffectDisposition::NeedsApproval(cap) => {
                ctx.push_tool_audit(ToolAuditRecord::approval_required(
                    &state_label,
                    *call_id,
                    name,
                    capability,
                ));
                let _ = sink
                    .emit(Event::ToolApprovalRequired {
                        call_id: *call_id,
                        capability: cap,
                        description: format!("tool '{}' requires approval for {cap:?}", name),
                    })
                    .await;
            }
        }
    }
}

fn publish<M: Machine>(
    status_tx: &watch::Sender<RuntimeStatus>,
    audit: &Arc<Mutex<Vec<AuditRecord>>>,
    ctx: &Context,
    hsm: &Hsm<M>,
    processed: u64,
    last_error: Option<String>,
) {
    if let Ok(mut guard) = audit.lock() {
        guard.clone_from(&ctx.audit);
    }
    let _ = status_tx.send(RuntimeStatus {
        state_label: hsm.state_label(),
        done: hsm.is_done(),
        last_error,
        processed,
    });
}

// ── ErasedRuntime ─────────────────────────────────────────────────────────────

/// A handle to a running kernel backed by a `Box<dyn ErasedMachine>`.
///
/// Analogous to [`Runtime<M>`] but without the generic type parameter. Use
/// this when the concrete machine type is selected at runtime (e.g. via
/// `sven_machines::ModeRegistry`).
pub struct ErasedRuntime {
    sink: EventSink,
    obs: ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    trail: AuditTrailHandle,
    capture_tx: mpsc::Sender<oneshot::Sender<Snapshot>>,
    handle: AbortOnDrop<ErasedReport>,
}

impl ErasedRuntime {
    /// Spawns the single consumer task for `machine`.
    ///
    /// `queue_depth` bounds the event mpsc channel. Initial-entry effects are
    /// validated and executed inside the spawned task.
    pub fn spawn<E>(
        machine: Box<dyn ErasedMachine>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        Self::spawn_with_children(machine, ctx, policy, executor, queue_depth, None)
    }

    /// Like [`spawn`](Self::spawn) but with an optional [`ChildSpawner`] that
    /// services [`Effect::InstantiateSubmachine`] by running children
    /// concurrently. Pass `None` for the historical (no submachine) behavior.
    pub fn spawn_with_children<E>(
        machine: Box<dyn ErasedMachine>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
        child_spawner: Option<Arc<dyn ChildSpawner>>,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        Self::spawn_with_audit_trail(
            machine,
            ctx,
            policy,
            executor,
            queue_depth,
            child_spawner,
            AuditTrailHandle::new(),
        )
    }

    /// Like [`spawn_with_children`](Self::spawn_with_children) but mirroring
    /// the audit trail into a caller-supplied [`AuditTrailHandle`].
    ///
    /// Share a clone of `trail` with an audit-persisting executor: the
    /// consumer task syncs the handle before and after every dispatch's
    /// effects and then executes
    /// [`Effect::PersistAudit`](sven_hsm::Effect::PersistAudit) itself, so every
    /// dispatch/tool/rejection record — including those of the final,
    /// terminal dispatch — reaches the durable log without any machine
    /// having to emit `PersistAudit`. Executors without an audit slot ignore
    /// the effect.
    pub fn spawn_with_audit_trail<E>(
        machine: Box<dyn ErasedMachine>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
        child_spawner: Option<Arc<dyn ChildSpawner>>,
        trail: AuditTrailHandle,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        let (tx, rx) = mpsc::channel::<Event>(queue_depth.max(1));
        let (status_tx, status_rx) = watch::channel(RuntimeStatus::default());
        let (capture_tx, capture_rx) = mpsc::channel::<oneshot::Sender<Snapshot>>(1);
        let sink = EventSink { tx };
        let obs = ObservationSink::new(OBSERVATION_CAPACITY);

        let handle = tokio::spawn(erased_consumer_loop(
            machine,
            ctx,
            policy,
            executor,
            rx,
            capture_rx,
            sink.clone(),
            obs.clone(),
            status_tx,
            trail.clone(),
            child_spawner,
        ));

        Self {
            sink,
            obs,
            status_rx,
            trail,
            capture_tx,
            handle: AbortOnDrop::new(handle),
        }
    }

    /// A sink for posting events into this runtime (cloneable; share with executors).
    #[must_use]
    pub fn sink(&self) -> EventSink {
        self.sink.clone()
    }

    /// A clone of this runtime's outward observation sink.
    #[must_use]
    pub fn observations(&self) -> ObservationSink {
        self.obs.clone()
    }

    /// Subscribes a new receiver to this runtime's outward observation stream.
    #[must_use]
    pub fn subscribe_observations(&self) -> tokio::sync::broadcast::Receiver<UiEvent> {
        self.obs.subscribe()
    }

    /// Posts an event, awaiting queue capacity. Returns `false` on shutdown.
    pub async fn post(&self, event: Event) -> bool {
        self.sink.emit(event).await
    }

    /// The latest published status snapshot.
    #[must_use]
    pub fn status(&self) -> RuntimeStatus {
        self.status_rx.borrow().clone()
    }

    /// A fresh receiver for status updates (watch channel).
    #[must_use]
    pub fn status_watch(&self) -> watch::Receiver<RuntimeStatus> {
        self.status_rx.clone()
    }

    /// A snapshot of the audit trail accumulated so far.
    #[must_use]
    pub fn audit_snapshot(&self) -> Vec<AuditRecord> {
        self.trail.records()
    }

    /// The shared audit-trail mirror this runtime publishes into.
    #[must_use]
    pub fn audit_trail(&self) -> AuditTrailHandle {
        self.trail.clone()
    }

    /// Waits until the machine's state label equals `label` or is terminal.
    pub async fn wait_for_state(&self, label: &str) {
        let mut rx = self.status_rx.clone();
        let _ = rx
            .wait_for(|s| s.state_label == label || s.done)
            .await
            .map(|_| ());
    }

    /// Waits until the machine reaches a terminal state.
    pub async fn wait_done(&self) {
        let mut rx = self.status_rx.clone();
        let _ = rx.wait_for(|s| s.done).await.map(|_| ());
    }

    /// Aborts the consumer task (best-effort shutdown, no drain).
    pub fn abort(&self) {
        self.handle.abort();
    }

    /// Detaches the consumer task: it keeps running after this handle is
    /// dropped.
    ///
    /// Sessions are normally owned — dropping the handle aborts the task and
    /// releases the machine, context, executor and conversation store with it.
    /// Some callers instead hand a cheap [`RuntimeHandle`] to a long-lived
    /// service (`ControlService`, the headless CI runner) and let the owning
    /// handle go out of scope, expecting the kernel to keep serving. Those
    /// callers must say so, because the two cases are indistinguishable at the
    /// drop site and the difference is a live session versus a dead one.
    pub fn detach(self) {
        // Dropping a `JoinHandle` detaches its task, which is the point.
        std::mem::drop(self.handle.disarm());
    }

    /// Captures the machine's current state and context without stopping it.
    ///
    /// The consumer task owns its context, so this asks for a copy and waits
    /// for it to be handed back. The request is served only once the event
    /// queue has drained, so the snapshot always shows the machine at rest
    /// rather than part-way through a turn.
    ///
    /// Callers relying on this to capture the end of a turn depend on the turn
    /// executor's ordering guarantee: the inward completion event is posted
    /// *before* the outward `UiEvent::TurnComplete` that tells a caller the
    /// turn is over. That is what puts the completion event in the queue ahead
    /// of the capture request.
    ///
    /// Returns `None` if the consumer task has already stopped.
    pub async fn capture(&self) -> Option<Snapshot> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.capture_tx.send(reply_tx).await.ok()?;
        reply_rx.await.ok()
    }

    /// Awaits the consumer task and returns the final context.
    ///
    /// # Errors
    ///
    /// Returns the join error if the task panicked or was aborted.
    pub async fn join(self) -> std::result::Result<ErasedReport, tokio::task::JoinError> {
        self.handle.disarm().await
    }
}

#[allow(clippy::too_many_arguments)]
async fn erased_consumer_loop<E>(
    mut machine: Box<dyn ErasedMachine>,
    mut ctx: Context,
    policy: PermissionPolicy,
    mut executor: E,
    mut rx: mpsc::Receiver<Event>,
    mut capture_rx: mpsc::Receiver<oneshot::Sender<Snapshot>>,
    sink: EventSink,
    obs: ObservationSink,
    status_tx: watch::Sender<RuntimeStatus>,
    trail: AuditTrailHandle,
    child_spawner: Option<Arc<dyn ChildSpawner>>,
) -> ErasedReport
where
    E: EffectExecutor + 'static,
{
    let mut processed: u64 = 0;
    let mut last_error: Option<String> = None;
    let mut children = Children::new(child_spawner);
    // Dispatched `CallTool` effects whose result event has not re-entered the
    // queue yet. Tool execution is spawned concurrently by the executor, so an
    // empty event queue does not mean the machine has nothing left to do -
    // quiescence-based captures must wait these out. Effect- and event-derived
    // only: replay reproduces the same set without any live executor state.
    let mut inflight_tools: HashSet<ToolCallId> = HashSet::new();

    let init_effects = machine.init(&mut ctx);
    // Mirror the audit trail *before* running effects so a `PersistAudit`
    // effect in this batch sees the records of the dispatch that emitted it.
    trail.sync_from(&ctx);
    run_effects(
        &policy,
        StateLabel(machine.state_label()),
        machine.state_label(),
        &mut ctx,
        &mut executor,
        &sink,
        &obs,
        EventKind::Init,
        init_effects,
        &mut last_error,
        &mut inflight_tools,
        &mut children,
    )
    .await;

    publish_erased(
        &status_tx,
        &trail,
        &ctx,
        machine.state_label(),
        machine.is_done(),
        processed,
        last_error.clone(),
    );

    // Runtime-driven audit persistence: flush the (freshly synced) trail
    // after every dispatch's effects have run, so the durable log also
    // contains the tool-audit and rejection records those effects produced.
    // Machines never need to emit `PersistAudit` themselves; executors
    // without an audit slot ignore it.
    executor.execute(Effect::PersistAudit, &sink, &obs).await;

    if machine.is_done() {
        return ErasedReport {
            state_label: machine.state_label(),
            ctx,
        };
    }

    loop {
        // A capture is served only when no event is pending, no dispatched
        // tool call is still awaiting its result, and never mid-dispatch.
        // `biased` is load-bearing, not a fairness preference: an unbiased
        // select would sometimes hand out a snapshot taken while the machine
        // still had queued work, capturing a transient mid-turn state.
        // Resuming from one of those would drop the agent back into a state
        // that ignores the next user message, and it would do so only
        // occasionally. The tool-call guard is what makes an empty queue a
        // true "nothing left to do" signal: tool executors spawn their work
        // concurrently, so between dispatching a `CallTool` and receiving its
        // result the queue is empty while the turn is very much still running.
        let event = tokio::select! {
            biased;
            event = rx.recv() => match event {
                Some(event) => event,
                None => break,
            },
            Some(reply) = capture_rx.recv(), if inflight_tools.is_empty() => {
                let _ = reply.send(Snapshot {
                    state: machine.state_label(),
                    context: ctx.clone(),
                });
                continue;
            }
        };
        // Retire in-flight calls whose result just arrived, before the
        // dispatch that consumes it.
        match &event {
            Event::ToolSucceeded { call_id, .. } | Event::ToolFailed { call_id, .. } => {
                inflight_tools.remove(call_id);
            }
            _ => {}
        }
        note_child_completion(&mut children, &event);
        let outcome = machine.dispatch(&event, &mut ctx);
        let event_kind = outcome.event;
        obs.emit(UiEvent::Transition {
            from: outcome.from.clone(),
            to: outcome.to.clone(),
            event: format!("{:?}", event_kind),
        });
        let effects = outcome.effects;
        // Mirror the audit trail *before* running effects (see init above).
        trail.sync_from(&ctx);
        run_effects(
            &policy,
            StateLabel(machine.state_label()),
            machine.state_label(),
            &mut ctx,
            &mut executor,
            &sink,
            &obs,
            event_kind,
            effects,
            &mut last_error,
            &mut inflight_tools,
            &mut children,
        )
        .await;

        processed += 1;
        publish_erased(
            &status_tx,
            &trail,
            &ctx,
            machine.state_label(),
            machine.is_done(),
            processed,
            last_error.clone(),
        );

        // Flush the audit trail after every dispatch (see the init flush
        // above). Because `publish_erased` has just re-synced the trail,
        // this batch includes the tool/rejection records that `run_effects`
        // pushed — including those of a *terminal* dispatch, which would
        // otherwise never reach the durable log.
        executor.execute(Effect::PersistAudit, &sink, &obs).await;

        if machine.is_done() {
            break;
        }
    }

    ErasedReport {
        state_label: machine.state_label(),
        ctx,
    }
}

fn publish_erased(
    status_tx: &watch::Sender<RuntimeStatus>,
    trail: &AuditTrailHandle,
    ctx: &Context,
    state_label: String,
    done: bool,
    processed: u64,
    last_error: Option<String>,
) {
    trail.sync_from(ctx);
    let _ = status_tx.send(RuntimeStatus {
        state_label,
        done,
        last_error,
        processed,
    });
}
