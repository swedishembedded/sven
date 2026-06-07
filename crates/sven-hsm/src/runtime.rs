//! The tokio Active Object runtime.
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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use serde_json::Value;

use crate::audit::AuditRecord;
use crate::context::Context;
use crate::effect::Effect;
use crate::event::{Event, EventKind, InternalEvent};
use crate::ids::{MachineId, TimerId};
use crate::machine::Machine;
use crate::observation::{ObservationSink, UiEvent};
use crate::permissions::{validate_effects_are_allowed, PermissionPolicy};
use crate::Hsm;

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

/// A monotonic, awaitable clock. Abstracted so timeouts are deterministic in
/// tests.
///
/// The fundamental operation is [`sleep_until`](Clock::sleep_until) with an
/// *absolute* deadline. Schedulers compute the deadline once, synchronously, at
/// scheduling time; this is what makes a [`VirtualClock`] race-free even when
/// time is advanced before the sleeping task starts.
#[async_trait]
pub trait Clock: Send + Sync + 'static {
    /// Time elapsed since the clock's epoch.
    fn now(&self) -> Duration;
    /// Resolves once clock-time reaches `deadline`.
    async fn sleep_until(&self, deadline: Duration);
    /// Resolves once `duration` of clock-time has elapsed from now.
    async fn sleep(&self, duration: Duration) {
        let deadline = self.now() + duration;
        self.sleep_until(deadline).await;
    }
}

/// Real wall-clock time backed by `tokio::time`.
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    /// Creates a clock whose epoch is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    async fn sleep_until(&self, deadline: Duration) {
        let now = self.now();
        if deadline > now {
            tokio::time::sleep(deadline - now).await;
        }
    }
}

/// A manually-driven virtual clock for deterministic timer tests.
///
/// Time starts at zero and only moves when [`advance`](VirtualClock::advance) is
/// called; sleeping tasks wake the instant the virtual time reaches their
/// deadline. Implemented with a `watch` channel so wakeups are edge-triggered
/// rather than polled.
#[derive(Clone)]
pub struct VirtualClock {
    tx: Arc<watch::Sender<Duration>>,
}

impl VirtualClock {
    /// Creates a virtual clock at time zero.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(Duration::ZERO);
        Self { tx: Arc::new(tx) }
    }

    /// Advances virtual time by `delta`, waking any sleepers whose deadline has
    /// now passed.
    pub fn advance(&self, delta: Duration) {
        self.tx.send_modify(|t| *t += delta);
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Clock for VirtualClock {
    fn now(&self) -> Duration {
        *self.tx.borrow()
    }

    async fn sleep_until(&self, deadline: Duration) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow() >= deadline {
                return;
            }
            if rx.changed().await.is_err() {
                return; // clock dropped
            }
        }
    }
}

/// Schedules one-shot timeouts that post `Event::Timeout { timer_id }` when they
/// elapse, using an injected [`Clock`]. Cancellation aborts the pending task.
pub struct TimerService {
    clock: Arc<dyn Clock>,
    sink: EventSink,
    tasks: HashMap<TimerId, JoinHandle<()>>,
}

impl TimerService {
    /// Creates a timer service that fires events into `sink` using `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, sink: EventSink) -> Self {
        Self {
            clock,
            sink,
            tasks: HashMap::new(),
        }
    }

    /// Schedules `timer_id` to fire after `duration` of clock-time. A timer with
    /// the same id is replaced.
    pub fn schedule(&mut self, timer_id: TimerId, duration: Duration) {
        let clock = Arc::clone(&self.clock);
        let sink = self.sink.clone();
        // Compute the absolute deadline now, synchronously, so advancing a
        // VirtualClock before the spawned task starts cannot be missed.
        let deadline = self.clock.now() + duration;
        let handle = tokio::spawn(async move {
            clock.sleep_until(deadline).await;
            let _ = sink.emit(Event::timeout(timer_id)).await;
        });
        if let Some(old) = self.tasks.insert(timer_id, handle) {
            old.abort();
        }
    }

    /// Cancels a scheduled timer, if present.
    pub fn cancel(&mut self, timer_id: TimerId) {
        if let Some(handle) = self.tasks.remove(&timer_id) {
            handle.abort();
        }
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

/// Tracks the set of child submachines currently in flight for a parent loop.
///
/// The parent machine drives aggregation (it owns the append-only thread), so
/// the kernel only needs a lightweight liveness map: insert on spawn, remove on
/// [`InternalEvent::SubmachineCompleted`]. Keeping it here gives the runtime an
/// authoritative concurrent-child count for observability and shutdown.
type ChildRegistry = HashMap<MachineId, ()>;

/// Pulls [`Effect::InstantiateSubmachine`] out of an effect batch and hands each
/// to the `spawner`, returning the remaining (non-child) effects for the normal
/// validate-then-execute path.
///
/// When no spawner is configured the instantiate effects are left in place so
/// they flow to the [`EffectExecutor`] (preserving the historical no-op), which
/// keeps every existing `Runtime`/`ErasedRuntime` caller behaving unchanged.
async fn spawn_children(
    effects: Vec<Effect>,
    spawner: &Option<Arc<dyn ChildSpawner>>,
    children: &mut ChildRegistry,
    sink: &EventSink,
) -> Vec<Effect> {
    let Some(spawner) = spawner else {
        return effects;
    };
    let mut remaining = Vec::with_capacity(effects.len());
    for effect in effects {
        match effect {
            Effect::InstantiateSubmachine { machine, descriptor } => {
                children.insert(machine, ());
                spawner.spawn_child(machine, descriptor, sink.clone()).await;
            }
            other => remaining.push(other),
        }
    }
    remaining
}

/// Drops a completed child from the registry so the parent's concurrent-child
/// count stays accurate.
fn note_child_completion(children: &mut ChildRegistry, event: &Event) {
    if let Event::Internal(InternalEvent::SubmachineCompleted { machine, .. }) = event {
        if let Ok(uuid) = uuid::Uuid::parse_str(machine) {
            children.remove(&MachineId::from_uuid(uuid));
        }
    }
}

/// An observable snapshot of the running machine, published after every
/// dispatch.
#[derive(Clone, Debug, Default)]
pub struct RuntimeStatus {
    /// Current state label.
    pub state_label: String,
    /// `true` once the machine reaches a terminal state.
    pub done: bool,
    /// Most recent permission-gate rejection message, if any.
    pub last_error: Option<String>,
    /// Number of events processed so far.
    pub processed: u64,
}

/// What the consumer task returns when it stops: the final machine and context.
pub struct RuntimeReport<M: Machine> {
    /// The machine in its final state.
    pub hsm: Hsm<M>,
    /// The final extended state (including the full audit trail).
    pub ctx: Context,
}

/// A handle to a running kernel. Post events, observe state, and join for the
/// final report.
pub struct Runtime<M: Machine> {
    sink: EventSink,
    obs: ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    handle: JoinHandle<RuntimeReport<M>>,
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
            handle,
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

    /// Awaits the consumer task and returns the final machine + context. The
    /// task ends when the machine reaches a terminal state or the queue closes.
    ///
    /// # Errors
    ///
    /// Returns the join error if the task panicked or was aborted.
    pub async fn join(self) -> std::result::Result<RuntimeReport<M>, tokio::task::JoinError> {
        self.handle.await
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
    let mut children: ChildRegistry = HashMap::new();

    // Initial transitions run inside the single consumer task, so their entry
    // effects go through the same validate-then-execute path as everything else.
    let init_effects = hsm.init(&mut ctx);
    let init_effects =
        spawn_children(init_effects, &child_spawner, &mut children, &sink).await;
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
        let effects =
            spawn_children(outcome.effects, &child_spawner, &mut children, &sink).await;
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
/// state) and, if allowed, hands each to the executor. On rejection, records a
/// `Rejected` audit entry and surfaces the error; **no** effect in the batch
/// executes.
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
) where
    S: std::fmt::Debug + Send,
    E: EffectExecutor,
{
    if effects.is_empty() {
        return;
    }
    // The permission gate is keyed on the machine's *current* state (the state
    // that now owns these in-flight effects). `state` is owned (not borrowed
    // from the Hsm) so the future stays `Send` across the executor await.
    match validate_effects_are_allowed(policy, &state, &effects, ctx) {
        Ok(()) => {
            *last_error = None;
            for effect in effects {
                executor.execute(effect, sink, obs).await;
            }
        }
        Err(err) => {
            let record = AuditRecord::rejected(state_label, event, &effects, err.to_string());
            ctx.audit.push(record);
            *last_error = Some(err.to_string());
            obs.emit(UiEvent::Error(err.to_string()));
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

// ── ErasedReport ──────────────────────────────────────────────────────────────

/// Like [`RuntimeReport`] but for type-erased machines. Returns the final
/// state label and extended context; the machine itself is consumed by the
/// loop.
pub struct ErasedReport {
    /// State label of the machine when it stopped.
    pub state_label: String,
    /// Final extended state (including the full audit trail).
    pub ctx: Context,
}

// ── StateLabel ────────────────────────────────────────────────────────────────

/// Wraps a state-label string so that `format!("{:?}", label)` returns the
/// label itself — without surrounding quotes — matching the key format used by
/// [`PermissionPolicy::state_label`].
///
/// Needed by [`ErasedRuntime`] to satisfy the `S: Debug` bound of
/// [`validate_effects_are_allowed`] when the concrete machine type is erased
/// and only a string label is available.
pub struct StateLabel(pub String);

impl std::fmt::Debug for StateLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── ErasedRuntime ─────────────────────────────────────────────────────────────

/// A handle to a running kernel backed by a `Box<dyn ErasedMachine>`.
///
/// Analogous to [`Runtime<M>`] but without the generic type parameter. Use
/// this when the concrete machine type is selected at runtime (e.g. via
/// `sven_core::ModeRegistry`).
pub struct ErasedRuntime {
    sink: EventSink,
    obs: ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    handle: JoinHandle<ErasedReport>,
}

impl ErasedRuntime {
    /// Spawns the single consumer task for `machine`.
    ///
    /// `queue_depth` bounds the event mpsc channel. Initial-entry effects are
    /// validated and executed inside the spawned task.
    pub fn spawn<E>(
        machine: Box<dyn crate::submachine::ErasedMachine>,
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
        machine: Box<dyn crate::submachine::ErasedMachine>,
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

        let handle = tokio::spawn(erased_consumer_loop(
            machine,
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
            handle,
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
        self.audit.lock().expect("audit mutex poisoned").clone()
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

    /// Awaits the consumer task and returns the final context.
    ///
    /// # Errors
    ///
    /// Returns the join error if the task panicked or was aborted.
    pub async fn join(self) -> std::result::Result<ErasedReport, tokio::task::JoinError> {
        self.handle.await
    }
}

#[allow(clippy::too_many_arguments)]
async fn erased_consumer_loop<E>(
    mut machine: Box<dyn crate::submachine::ErasedMachine>,
    mut ctx: Context,
    policy: PermissionPolicy,
    mut executor: E,
    mut rx: mpsc::Receiver<Event>,
    sink: EventSink,
    obs: ObservationSink,
    status_tx: watch::Sender<RuntimeStatus>,
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    child_spawner: Option<Arc<dyn ChildSpawner>>,
) -> ErasedReport
where
    E: EffectExecutor + 'static,
{
    let mut processed: u64 = 0;
    let mut last_error: Option<String> = None;
    let mut children: ChildRegistry = HashMap::new();

    let init_effects = machine.init(&mut ctx);
    let init_effects =
        spawn_children(init_effects, &child_spawner, &mut children, &sink).await;
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
    )
    .await;

    publish_erased(
        &status_tx,
        &audit,
        &ctx,
        machine.state_label(),
        machine.is_done(),
        processed,
        last_error.clone(),
    );

    if machine.is_done() {
        return ErasedReport {
            state_label: machine.state_label(),
            ctx,
        };
    }

    while let Some(event) = rx.recv().await {
        note_child_completion(&mut children, &event);
        let outcome = machine.dispatch(&event, &mut ctx);
        let event_kind = outcome.event;
        obs.emit(UiEvent::Transition {
            from: outcome.from.clone(),
            to: outcome.to.clone(),
            event: format!("{:?}", event_kind),
        });
        let effects =
            spawn_children(outcome.effects, &child_spawner, &mut children, &sink).await;
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
        )
        .await;

        processed += 1;
        publish_erased(
            &status_tx,
            &audit,
            &ctx,
            machine.state_label(),
            machine.is_done(),
            processed,
            last_error.clone(),
        );

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
    audit: &Arc<Mutex<Vec<AuditRecord>>>,
    ctx: &Context,
    state_label: String,
    done: bool,
    processed: u64,
    last_error: Option<String>,
) {
    if let Ok(mut guard) = audit.lock() {
        guard.clone_from(&ctx.audit);
    }
    let _ = status_tx.send(RuntimeStatus {
        state_label,
        done,
        last_error,
        processed,
    });
}
