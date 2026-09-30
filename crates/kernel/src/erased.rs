// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel runtime for a machine whose type is chosen at run time.

use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};

use sven_hsm::{
    AuditRecord, AuditTrailHandle, Context, Effect, ErasedMachine, ErasedReport, Event, EventKind,
    ObservationSink, PermissionPolicy, RuntimeStatus, Snapshot, StateLabel, UiEvent,
};

use crate::abort_on_drop::AbortOnDrop;
use crate::children::{ChildRun, ChildSpawner, Children};
use crate::{
    publish_cancelled, run_effects, CancelScope, EffectExecutor, EventSink, InFlight,
    OBSERVATION_CAPACITY,
};

/// A handle to a running kernel backed by a `Box<dyn ErasedMachine>`.
///
/// Analogous to [`Runtime<M>`](crate::Runtime) but without the generic type parameter. Use
/// this when the concrete machine type is selected at runtime (e.g. via
/// `sven_machines::ModeRegistry`).
pub struct ErasedRuntime {
    sink: EventSink,
    obs: ObservationSink,
    status_rx: watch::Receiver<RuntimeStatus>,
    trail: AuditTrailHandle,
    capture_tx: mpsc::Sender<oneshot::Sender<Snapshot>>,
    cancel: CancelScope,
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
        let children = Children::new(child_spawner, CancelScope::new(), None);
        Self::launch(machine, ctx, policy, executor, queue_depth, children, trail)
    }

    /// Starts `machine` as a child run under `run`, the terms its parent
    /// handed down (see [`ChildSpawner`]).
    ///
    /// The kernel enforces what it can see: the child's permission policy is
    /// `run.contract.policy`, the run stops when `run.cancel` is cancelled -
    /// by the parent, or by the contract's deadline, which is armed here - and
    /// any children it starts in turn inherit no more than `run.contract`. The
    /// budgets that belong to the child's turns (`max_tool_rounds`,
    /// `max_output_tokens`) are the spawner's to apply to the machine's
    /// context and executor, which the kernel does not own.
    pub fn spawn_child_run<E>(
        machine: Box<dyn ErasedMachine>,
        ctx: Context,
        executor: E,
        queue_depth: usize,
        child_spawner: Option<Arc<dyn ChildSpawner>>,
        run: ChildRun,
    ) -> Self
    where
        E: EffectExecutor + 'static,
    {
        let ChildRun { contract, cancel } = run;
        let timer = contract.deadline.map(|deadline| cancel.cancel_at(deadline));
        let policy = contract.policy.clone();
        let children = Children::new(child_spawner, cancel, Some(contract)).with_deadline(timer);
        Self::launch(
            machine,
            ctx,
            policy,
            executor,
            queue_depth,
            children,
            AuditTrailHandle::new(),
        )
    }

    fn launch<E>(
        machine: Box<dyn ErasedMachine>,
        ctx: Context,
        policy: PermissionPolicy,
        executor: E,
        queue_depth: usize,
        children: Children,
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
        let cancel = children.scope();

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
            children,
        ));

        Self {
            sink,
            obs,
            status_rx,
            trail,
            capture_tx,
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
    /// `done` with [`RUN_CANCELLED`](crate::RUN_CANCELLED) in `last_error`.
    pub fn cancel(&self) {
        self.cancel.cancel();
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
    /// Some callers instead hand a cheap `RuntimeHandle` (`sven-bootstrap`) to a long-lived
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
    mut children: Children,
) -> ErasedReport
where
    E: EffectExecutor + 'static,
{
    let mut processed: u64 = 0;
    let mut last_error: Option<String> = None;
    let cancel = children.scope();
    // Dispatched `CallTool` effects whose result event has not re-entered the
    // queue yet. Tool execution is spawned concurrently by the executor, so an
    // empty event queue does not mean the machine has nothing left to do -
    // quiescence-based captures must wait these out. Effect- and event-derived
    // only: replay reproduces the same set without any live executor state.
    let mut inflight = InFlight::default();

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
        &mut inflight,
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
            () = cancel.cancelled() => break,
            event = rx.recv() => match event {
                Some(event) => event,
                None => break,
            },
            Some(reply) = capture_rx.recv(), if inflight.is_empty() => {
                let _ = reply.send(Snapshot {
                    state: machine.state_label(),
                    context: ctx.clone(),
                });
                continue;
            }
        };
        // Retire in-flight calls whose result just arrived, before the
        // dispatch that consumes it.
        inflight.retire(&event);
        children.note_completion(&event);
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
        tokio::select! {
            biased;
            () = cancel.cancelled() => {}
            () = run_effects(
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
                &mut inflight,
                &mut children,
            ) => {}
        }

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

        if machine.is_done() || cancel.is_cancelled() {
            break;
        }
    }
    if cancel.is_cancelled() {
        publish_cancelled(&status_tx);
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
