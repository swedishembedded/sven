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
use rt::{Instant, MaybeSend};
use std::collections::{HashMap, HashSet};
pub use sven_rt as rt;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch};

use sven_hsm::{
    classify, validate_effects_are_allowed, ApprovalId, AuditRecord, Context, Effect,
    EffectDisposition, Event, EventKind, ObservationSink, PermissionPolicy, RuntimeStatus,
    StateLabel, ToolAuditRecord, ToolCallId, ToolCapability, UiEvent,
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
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
pub trait EffectExecutor: MaybeSend {
    /// Performs `effect`.
    ///
    /// May post zero or more result [`Event`]s back through `sink` (the inward
    /// plane) and emit zero or more [`UiEvent`]s through `obs` (the outward
    /// streaming plane). Streaming output (token deltas, tool progress, usage)
    /// goes to `obs`; exactly the completion fact that advances the machine
    /// goes to `sink`.
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink);

    /// Hands the executor the scope that stops its run, once, before the
    /// first effect.
    ///
    /// An executor that starts work outliving its `execute` call (a spawned
    /// tool call) stops that work when `scope` is cancelled. The kernel
    /// cancels the scope when the run ends for any reason - a bound, a
    /// cancellation, or the run being dropped - so nothing started on a
    /// run's behalf outlives it. The default does nothing: an executor whose
    /// `execute` finishes its work has nothing to stop.
    fn bind_cancel(&mut self, _scope: &CancelScope) {}

    /// Whether an approval request this executor accepts is parked rather
    /// than answered: it waits in the context until a decision event arrives
    /// from outside the run, which may be much later. The kernel then does
    /// not count the request as work in flight, so a capture is served while
    /// it is pending - the same contract as a parked question. The default is
    /// `false`: the executor holds the request open and answers it.
    fn parks_approvals(&self) -> bool {
        false
    }

    /// Tells the executor that the permission gate refused the tool call
    /// `call_id` outright, because `capability` is not permitted in the
    /// session's current state. A refused call is never handed to
    /// [`execute`](Self::execute); the machine is told by `ToolFailed`. This
    /// is how an executor that records what happened to calls learns of the
    /// ones it will never run. The default does nothing.
    fn gate_refused(&mut self, _call_id: ToolCallId, _capability: ToolCapability, _reason: &str) {}
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl EffectExecutor for Box<dyn EffectExecutor> {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        self.as_mut().execute(effect, sink, obs).await;
    }

    fn bind_cancel(&mut self, scope: &CancelScope) {
        self.as_mut().bind_cancel(scope);
    }

    fn parks_approvals(&self) -> bool {
        self.as_ref().parks_approvals()
    }

    fn gate_refused(&mut self, call_id: ToolCallId, capability: ToolCapability, reason: &str) {
        self.as_mut().gate_refused(call_id, capability, reason);
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
/// tracked, because parking exists so a run can stop and resume when it is
/// answered; neither is an approval request that an executor which
/// [parks approvals](EffectExecutor::parks_approvals) accepted.
struct InFlight {
    /// Dispatched calls awaiting their result, with the moment they were
    /// dispatched and the audit record that announced them.
    tools: HashMap<ToolCallId, (Instant, ToolAuditRecord)>,
    approvals: HashSet<ApprovalId>,
    parks_approvals: bool,
}

impl InFlight {
    /// Nothing in flight; `parks_approvals` is the executor's answer to
    /// [`EffectExecutor::parks_approvals`].
    fn new(parks_approvals: bool) -> Self {
        Self {
            tools: HashMap::new(),
            approvals: HashSet::new(),
            parks_approvals,
        }
    }

    /// Records an approval request handed to the executor. A parked request
    /// is not work in flight: its decision arrives from outside the run.
    fn hold_approval(&mut self, approval_id: ApprovalId) {
        if !self.parks_approvals {
            self.approvals.insert(approval_id);
        }
    }

    fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.approvals.is_empty()
    }

    /// Records a dispatched call, timing it from now.
    fn dispatch_tool(&mut self, started: ToolAuditRecord) {
        self.tools
            .insert(started.call_id, (Instant::now(), started));
    }

    /// Retires what `event` answers, before the dispatch that consumes it.
    ///
    /// Returns the audit record of a call that just ran to its result, for
    /// the caller to append to the context. A call refused by the gate was
    /// never dispatched, so its `ToolFailed` yields none (it has a `Denied`
    /// record already).
    fn retire(&mut self, event: &Event) -> Option<ToolAuditRecord> {
        match event {
            Event::ToolSucceeded {
                call_id,
                observation,
            } => {
                let (at, started) = self.tools.remove(call_id)?;
                return Some(ToolAuditRecord::finished(
                    &started,
                    Ok(observation),
                    elapsed_ms(at),
                ));
            }
            Event::ToolFailed { call_id, error } => {
                let (at, started) = self.tools.remove(call_id)?;
                return Some(ToolAuditRecord::finished(
                    &started,
                    Err(error.as_str()),
                    elapsed_ms(at),
                ));
            }
            // A parked call has answered as far as the kernel is concerned:
            // it resumes only from `HumanAnswered`, however much later. It
            // has not finished, so it gets no result record.
            Event::QuestionAsked { call_id, .. } => {
                self.tools.remove(call_id);
            }
            Event::HumanApproved { approval_id } | Event::HumanRejected { approval_id, .. } => {
                self.approvals.remove(approval_id);
            }
            _ => {}
        }
        None
    }
}

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
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
                        inflight.hold_approval(*approval_id);
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
                let started = ToolAuditRecord::started(&state_label, *call_id, name, capability);
                ctx.push_tool_audit(started.clone());
                *last_error = None;
                // Register before execution so the consumer loop holds the
                // quiescence signal until the result event re-enters the
                // queue, no matter how the executor runs the tool.
                inflight.dispatch_tool(started);
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
                executor.gate_refused(*call_id, capability, &reason);
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
