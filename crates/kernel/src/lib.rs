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
//! A crate separate from `sven-hsm` so the kernel's pure vocabulary
//! (`Machine`, `Effect`, `Event`, `Context`, `PermissionPolicy`,
//! `AuditRecord`) has no tokio dependency of its own — only the active-object
//! execution engine that drives it does.
//! `sven-hsm`'s outward observation plane (`ObservationSink`, a broadcast
//! channel) stays in `sven-hsm`: it's part of the kernel's event vocabulary,
//! not the execution engine, and placing it here would force a
//! `sven-kernel` dependency onto every one of `UiEvent`'s many consumers for
//! no benefit.

mod abort_on_drop;
use abort_on_drop::AbortOnDrop;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use sven_hsm::{
    classify, validate_effects_are_allowed, ApprovalId, AuditRecord, Context, Effect,
    EffectDisposition, Event, EventKind, Hsm, Machine, ObservationSink, PermissionPolicy,
    RuntimeReport, RuntimeStatus, StateLabel, ToolAuditRecord, ToolCallId, UiEvent,
};

mod cancel;
pub use cancel::{CancelScope, DeadlineTimer};
mod children;
use children::Children;
pub use children::{ChildRun, ChildSpawner};
mod clock;
mod erased;
pub use clock::{Clock, SystemClock, TimerService, VirtualClock};
pub use erased::ErasedRuntime;
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

    /// Resolves once the kernel has stopped - its run finished, was
    /// cancelled or was dropped - so nothing posted here can be dispatched
    /// any more. Lets work done on the kernel's behalf, such as waiting for a
    /// person's answer, give up when nobody is left to receive it.
    pub async fn closed(&self) {
        self.tx.closed().await;
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

/// Work the kernel has started and whose answer has not re-entered the
/// queue yet. While any is outstanding an empty queue is not "nothing left to
/// do", so a capture is not served.
///
/// A tool call is outstanding from dispatch to its result. An approval
/// request is outstanding until `HumanApproved`/`HumanRejected` for it: the
/// host may answer at once or after asking someone, and either way the turn
/// is not over. A parked question (`RequestHumanAnswer`) is deliberately not
/// tracked - parking exists so a run can stop and resume when it is answered.
#[derive(Default)]
struct InFlight {
    tools: HashSet<ToolCallId>,
    approvals: HashSet<ApprovalId>,
}

impl InFlight {
    fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.approvals.is_empty()
    }

    /// Retires what `event` answers, before the dispatch that consumes it.
    fn retire(&mut self, event: &Event) {
        match event {
            // A parked call has answered as far as the kernel is concerned:
            // it resumes only from `HumanAnswered`, however much later.
            Event::ToolSucceeded { call_id, .. }
            | Event::ToolFailed { call_id, .. }
            | Event::QuestionAsked { call_id, .. } => {
                self.tools.remove(call_id);
            }
            Event::HumanApproved { approval_id } | Event::HumanRejected { approval_id } => {
                self.approvals.remove(approval_id);
            }
            _ => {}
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
    cancel: CancelScope,
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
        let cancel = CancelScope::new();
        let children = Children::new(child_spawner, cancel.clone(), None);

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
            children,
            cancel.clone(),
        ));

        Self {
            sink,
            obs,
            status_rx,
            audit,
            cancel,
            handle: AbortOnDrop::new(handle),
        }
    }

    /// The scope that stops this run; child runs it starts derive from it.
    #[must_use]
    pub fn cancel_scope(&self) -> CancelScope {
        self.cancel.clone()
    }

    /// Stops the run: no further event is dispatched, the effects in flight
    /// are dropped, every live child run is cancelled, and the status turns
    /// `done` with the reason in `last_error`.
    pub fn cancel(&self) {
        self.cancel.cancel();
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
    mut children: Children,
    cancel: CancelScope,
) -> RuntimeReport<M>
where
    M: Machine + Send + 'static,
    M::State: Send,
    E: EffectExecutor + 'static,
{
    let mut processed: u64 = 0;
    let mut last_error: Option<String> = None;
    // Same in-flight registry as the erased loop. The typed runtime exposes no
    // quiescence-based capture, so the set is only fed; keeping the bookkeeping
    // identical means run_effects has one contract.
    let mut inflight = InFlight::default();

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
        &mut inflight,
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

    loop {
        let event = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            event = rx.recv() => match event {
                Some(event) => event,
                None => break,
            },
        };
        children.note_completion(&event);
        let outcome = hsm.dispatch(&event, &mut ctx);
        let event_kind = outcome.event;
        // Emit the transition trace on the outward plane after every dispatch.
        obs.emit(UiEvent::Transition {
            from: outcome.from.clone(),
            to: outcome.to.clone(),
            event: format!("{:?}", event_kind),
        });
        let effects = outcome.effects;
        let state = hsm.state();
        let state_label = hsm.state_label();
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            () = run_effects(
                &policy,
                state,
                state_label,
                &mut ctx,
                &mut executor,
                &sink,
                &obs,
                event_kind,
                effects,
                &mut last_error,
                &mut inflight,
                &mut children,
            ) => {}
        }

        processed += 1;
        publish(
            &status_tx,
            &audit,
            &ctx,
            &hsm,
            processed,
            last_error.clone(),
        );

        if hsm.is_done() || cancel.is_cancelled() {
            break;
        }
    }
    if cancel.is_cancelled() {
        publish_cancelled(&status_tx);
    }

    RuntimeReport { hsm, ctx }
}

/// Marks a cancelled run finished, so a caller waiting on `done` wakes. The
/// machine's own state label is left as it was when the run stopped.
fn publish_cancelled(status_tx: &watch::Sender<RuntimeStatus>) {
    status_tx.send_modify(|status| {
        status.done = true;
        status.last_error = Some(RUN_CANCELLED.to_string());
    });
}

/// The `last_error` of a run that was cancelled before its machine finished.
pub const RUN_CANCELLED: &str = "run cancelled";

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
    inflight: &mut InFlight,
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
        let label = StateLabel(state_label.clone());
        match validate_effects_are_allowed(policy, &state, &other_effects, ctx) {
            Ok(()) => {
                *last_error = None;
                for effect in other_effects {
                    if let Effect::RequestHumanApproval { approval_id, .. } = &effect {
                        inflight.approvals.insert(*approval_id);
                    }
                    children
                        .execute(effect, policy, &label, executor, sink, obs)
                        .await;
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
                inflight.tools.insert(*call_id);
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
