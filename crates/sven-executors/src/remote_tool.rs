//! Remote tool effect executor — the cloud half of "remote hands".
//!
//! Handles [`Effect::CallTool`] by forwarding it as a [`sven_wire`]
//! [`ToolCallRequest`] to a customer companion that dialled **out** to the
//! control plane, then awaiting the matching [`ToolCallResult`] and emitting
//! [`Event::ToolSucceeded`] / [`Event::ToolFailed`] — the exact same event
//! shape [`crate::ToolExecutor`] produces locally, so machines cannot tell
//! whether a tool ran in the cloud or on the customer's box.
//!
//! ## Wiring
//!
//! The executor is transport-agnostic: it writes [`TetherMessage`]s into an
//! outbound [`mpsc::Sender`] (the tether writer task serializes them onto the
//! wire) and receives results through a cloneable [`RemoteToolRouter`] handle
//! (the tether reader task calls [`RemoteToolRouter::deliver`] for every
//! `ToolCallResult` frame, and [`RemoteToolRouter::disconnect`] when the
//! companion drops).
//!
//! ```rust,ignore
//! let (to_companion_tx, to_companion_rx) = tokio::sync::mpsc::channel(64);
//! let exec = RemoteToolExecutor::new(to_companion_tx)
//!     .with_delegate(Box::new(default_composite));
//! let router = exec.router();
//! // tether reader task: router.deliver(result) / router.disconnect()
//! let builder = RuntimeBuilder::new(config, "chat")
//!     .with_effect_executor(Box::new(exec));
//! ```
//!
//! ## Never hang the kernel
//!
//! Each `CallTool` spawns an independent task (mirroring `ToolExecutor`'s
//! spawn-and-forget concurrency). Every failure path — send error because the
//! tether writer is gone, companion disconnect while a call is in flight, or
//! the per-call timeout elapsing — synthesizes `Event::ToolFailed`, so the
//! kernel always observes a terminal event for every dispatched call.
//!
//! ## Delegation
//!
//! Effects other than `CallTool` are handed to the optional delegate executor
//! (typically the default [`crate::CompositeExecutor`]); without a delegate
//! they are logged and dropped.
//!
//! ## Not yet turn-loop complete (parity gap)
//!
//! [`crate::ToolExecutor`] does two things this executor does **not**: it
//! resolves the wire `call_id` back to `(thread_id, original LLM call id)` and
//! **appends a `Message::tool_result` to the shared `ConversationStore`**
//! before emitting `ToolSucceeded`/`ToolFailed`, and it enforces a second
//! `allowed_capabilities` check. `loop_core` machines build their continuation
//! `CallLlm` from that store, not from the event payload. This executor only
//! emits the event, so a real turn-loop kernel driven solely through it would
//! send the provider an assistant `tool_call` with no matching `tool_result`
//! and be rejected for an unmatched `tool_use`. Until the store / call-id map
//! is shared with this executor, it can drive **one-shot** machines that read
//! the observation off the event, but it cannot yet drive a multi-turn
//! LLM loop. Wire it alongside (not instead of) `TurnExecutor` when a machine
//! needs conversation continuity.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use sven_hsm::{
    Effect, EffectExecutor, Event, EventSink, ObservationSink, ToolCallId, ToolCapability, UiEvent,
};
use sven_llm::ConversationStore;
use sven_model::Message;
use sven_wire::{TetherMessage, ToolCallRequest, ToolCallResult};
use tokio::sync::{mpsc, oneshot};

/// Shared `call_id → (thread_id, original_call_id)` registry, populated by the
/// `TurnExecutor` and shared with this executor so remote tool results land on
/// the correct conversation thread (closing the turn-loop parity gap).
type CallIdRegistry = Arc<Mutex<HashMap<ToolCallId, (String, String)>>>;

/// Default per-call timeout: generous, since remote tools may compile or run
/// test suites on the customer's machine.
pub const DEFAULT_REMOTE_TOOL_TIMEOUT: Duration = Duration::from_secs(300);

/// Pending calls keyed by their idempotent wire `call_id`.
type PendingMap = Arc<Mutex<HashMap<ToolCallId, oneshot::Sender<ToolCallResult>>>>;

/// Cloneable handle the tether transport uses to route companion messages
/// back into a [`RemoteToolExecutor`].
///
/// Obtain it via [`RemoteToolExecutor::router`] and hand it to the task that
/// reads frames from the companion connection.
#[derive(Clone, Default)]
pub struct RemoteToolRouter {
    pending: PendingMap,
}

impl RemoteToolRouter {
    /// Delivers a [`ToolCallResult`] received from the companion.
    ///
    /// Returns `true` if a call was awaiting this `call_id`; `false` for
    /// unknown/duplicate results (already timed out, replayed on reconnect),
    /// which are safe to ignore thanks to the idempotent `ToolCallId`.
    pub fn deliver(&self, result: ToolCallResult) -> bool {
        let waiter = self
            .pending
            .lock()
            .expect("remote tool pending map poisoned")
            .remove(&result.call_id);
        match waiter {
            Some(tx) => tx.send(result).is_ok(),
            None => {
                tracing::debug!(
                    call_id = %result.call_id,
                    "RemoteToolRouter: result for unknown call_id ignored"
                );
                false
            }
        }
    }

    /// Fails every in-flight call: the companion disconnected.
    ///
    /// Each awaiting task observes its `oneshot` sender being dropped and
    /// synthesizes [`Event::ToolFailed`] immediately (no need to wait for the
    /// per-call timeout).
    pub fn disconnect(&self) {
        let dropped: Vec<_> = self
            .pending
            .lock()
            .expect("remote tool pending map poisoned")
            .drain()
            .collect();
        if !dropped.is_empty() {
            tracing::warn!(
                count = dropped.len(),
                "RemoteToolRouter: companion disconnected with calls in flight"
            );
        }
        // Senders drop here; receivers see RecvError.
    }

    /// Number of calls currently awaiting a companion result.
    #[must_use]
    pub fn pending_calls(&self) -> usize {
        self.pending
            .lock()
            .expect("remote tool pending map poisoned")
            .len()
    }
}

/// Executes [`Effect::CallTool`] on a remote companion over the tether.
///
/// See the [module docs](self) for wiring and failure semantics.
pub struct RemoteToolExecutor {
    /// Outbound frames towards the companion (consumed by the tether writer).
    outbound: mpsc::Sender<TetherMessage>,
    /// Shared registry of calls awaiting a result.
    router: RemoteToolRouter,
    /// Per-call deadline; on expiry the call fails with `ToolFailed`.
    timeout: Duration,
    /// Globally-allowed capabilities (second-line defence check, mirroring
    /// [`crate::ToolExecutor`]). Empty ⇒ allow-all (rely solely on the kernel
    /// gate). A `CallTool` whose capability is not in a non-empty set is denied
    /// with `ToolFailed` and never forwarded to the companion.
    allowed_capabilities: HashSet<ToolCapability>,
    /// Receives every non-`CallTool` effect (typically the default composite).
    delegate: Option<Box<dyn EffectExecutor>>,
    /// Shared conversation store; when set (together with [`Self::call_id_to_thread`])
    /// remote tool results are appended to the thread the `TurnExecutor` wrote
    /// the assistant tool-call into, so a multi-turn LLM loop over remote hands
    /// sees the matching `tool_result` on the continuation call.
    store: Option<Arc<Mutex<ConversationStore>>>,
    /// Shared `call_id → (thread, original_id)` registry (see [`Self::store`]).
    call_id_to_thread: Option<CallIdRegistry>,
}

impl RemoteToolExecutor {
    /// Creates an executor that forwards tool calls into `outbound` with the
    /// [default timeout](DEFAULT_REMOTE_TOOL_TIMEOUT) and no delegate.
    #[must_use]
    pub fn new(outbound: mpsc::Sender<TetherMessage>) -> Self {
        Self {
            outbound,
            router: RemoteToolRouter::default(),
            timeout: DEFAULT_REMOTE_TOOL_TIMEOUT,
            delegate: None,
            store: None,
            call_id_to_thread: None,
            allowed_capabilities: HashSet::new(),
        }
    }

    /// Restricts which capabilities may be forwarded to the companion.
    ///
    /// Mirrors [`crate::ToolExecutor`]'s second-line capability check so the
    /// cloud kernel's `PermissionPolicy` is enforced (not merely advisory) for
    /// remote tools. An empty set (the default) allows all capabilities.
    #[must_use]
    pub fn with_allowed_capabilities(mut self, allowed: HashSet<ToolCapability>) -> Self {
        self.allowed_capabilities = allowed;
        self
    }

    /// Sets the per-call timeout.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Shares the `TurnExecutor`'s conversation store and `call_id → thread`
    /// registry so remote tool results are appended to the right thread as a
    /// `Message::tool_result` (matching the id the LLM assigned) before the
    /// continuation LLM call — the same bookkeeping [`crate::ToolExecutor`]
    /// does for local tools. Without this, a multi-turn kernel driven over
    /// remote hands would send the provider an assistant `tool_call` with no
    /// matching `tool_result`.
    #[must_use]
    pub fn with_shared_store(
        mut self,
        store: Arc<Mutex<ConversationStore>>,
        call_id_to_thread: CallIdRegistry,
    ) -> Self {
        self.store = Some(store);
        self.call_id_to_thread = Some(call_id_to_thread);
        self
    }

    /// Sets the executor that receives every effect other than `CallTool`
    /// (typically a [`crate::CompositeExecutor`] built without a tool slot).
    #[must_use]
    pub fn with_delegate(mut self, delegate: Box<dyn EffectExecutor>) -> Self {
        self.delegate = Some(delegate);
        self
    }

    /// Returns the router handle for the tether reader task.
    #[must_use]
    pub fn router(&self) -> RemoteToolRouter {
        self.router.clone()
    }
}

/// Maps a companion [`ToolCallResult`] onto the kernel event vocabulary,
/// mirroring how [`crate::ToolExecutor`] reports local [`ToolOutput`]s
/// (tool-level errors become `ToolFailed { error: content }`, successes
/// become `ToolSucceeded` with a string observation).
///
/// [`ToolOutput`]: sven_tools::ToolOutput
fn result_to_event(call_id: ToolCallId, result: ToolCallResult) -> Event {
    match (result.output, result.error) {
        (Some(output), _) => {
            if output.is_error {
                Event::ToolFailed {
                    call_id,
                    error: output.content,
                }
            } else {
                Event::ToolSucceeded {
                    call_id,
                    observation: serde_json::Value::String(output.content),
                }
            }
        }
        (None, Some(error)) => Event::ToolFailed { call_id, error },
        (None, None) => Event::ToolFailed {
            call_id,
            error: "companion returned a result with neither output nor error".into(),
        },
    }
}

/// Reports a terminal remote-tool [`Event`] the same way [`crate::ToolExecutor`]
/// reports a local one: emit the outward [`UiEvent::ToolFinished`] observation
/// (so operator/UI streams render the result), append a `Message::tool_result`
/// to the shared conversation thread when a mapping exists (so a multi-turn LLM
/// loop sees the matching result), and finally emit the inward `event`.
async fn report_remote_result(
    sink: &EventSink,
    obs: &ObservationSink,
    store: Option<&Arc<Mutex<ConversationStore>>>,
    mapping: Option<&(String, String)>,
    display_id: &str,
    name: &str,
    event: Event,
) {
    let (content, is_error) = match &event {
        Event::ToolSucceeded { observation, .. } => (
            observation
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| observation.to_string()),
            false,
        ),
        Event::ToolFailed { error, .. } => (error.clone(), true),
        _ => (String::new(), false),
    };

    obs.emit(UiEvent::ToolFinished {
        call_id: display_id.to_string(),
        name: name.to_string(),
        output: content.clone(),
        is_error,
    });

    if let (Some(store), Some((thread_id, orig_id))) = (store, mapping) {
        if let Ok(mut s) = store.lock() {
            let msg = if is_error {
                Message::tool_result(orig_id, format!("error: {content}"))
            } else {
                Message::tool_result(orig_id, &content)
            };
            s.append(thread_id, msg);
        }
    }

    let _ = sink.emit(event).await;
}

#[async_trait]
impl EffectExecutor for RemoteToolExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallTool {
            call_id,
            name,
            capability,
            args,
        } = effect
        else {
            // Not ours — hand to the delegate (default composite) if any.
            if let Some(delegate) = &mut self.delegate {
                delegate.execute(effect, sink, obs).await;
            } else {
                tracing::warn!(
                    kind = ?effect.kind(),
                    "RemoteToolExecutor: no delegate configured; dropping effect"
                );
            }
            return;
        };

        // Second-line capability check (defence-in-depth), mirroring
        // `ToolExecutor`: enforce the kernel's `allowed_capabilities` so the
        // cloud `PermissionPolicy` is not merely advisory for remote tools. A
        // disallowed capability fails with `ToolFailed` and is NEVER forwarded
        // to the companion. An empty set means allow-all.
        if !self.allowed_capabilities.is_empty()
            && !self.allowed_capabilities.contains(&capability)
        {
            tracing::warn!(
                tool = %name,
                ?capability,
                "RemoteToolExecutor: capability not in allow-list (kernel gate should have caught this)"
            );
            let _ = sink
                .emit(Event::ToolFailed {
                    call_id,
                    error: format!(
                        "capability {capability:?} is not permitted in the current context"
                    ),
                })
                .await;
            return;
        }

        // Register the waiter *before* sending so a fast companion cannot
        // race the result past the router.
        let (result_tx, result_rx) = oneshot::channel::<ToolCallResult>();
        self.router
            .pending
            .lock()
            .expect("remote tool pending map poisoned")
            .insert(call_id, result_tx);

        // Resolve the (thread, original_call_id) mapping before spawning, so
        // remote results land on the right thread using the exact id the LLM
        // assigned (the API rejects a `tool_result` whose id does not match the
        // preceding assistant `tool_call`). The display id also correlates the
        // outward `ToolFinished` observation with the earlier `ToolStarted`.
        let mapping = self
            .call_id_to_thread
            .as_ref()
            .and_then(|m| m.lock().ok().and_then(|g| g.get(&call_id).cloned()));
        let display_id = mapping
            .as_ref()
            .map(|(_, orig)| orig.clone())
            .unwrap_or_else(|| call_id.as_uuid().to_string());

        let request = ToolCallRequest {
            call_id,
            name: name.clone(),
            capability,
            args,
        };
        let outbound = self.outbound.clone();
        let pending = Arc::clone(&self.router.pending);
        let timeout = self.timeout;
        let sink = sink.clone();
        let obs = obs.clone();
        let store = self.store.clone();

        // Spawn-and-forget: the kernel's consumer loop returns immediately and
        // the result arrives back as an event, exactly like `ToolExecutor`.
        tokio::spawn(async move {
            tracing::debug!(tool = %name, %call_id, "RemoteToolExecutor: dispatching to companion");
            if outbound
                .send(TetherMessage::ToolCallRequest(request))
                .await
                .is_err()
            {
                pending
                    .lock()
                    .expect("remote tool pending map poisoned")
                    .remove(&call_id);
                let error = format!("companion disconnected before tool '{name}' was sent");
                report_remote_result(
                    &sink,
                    &obs,
                    store.as_ref(),
                    mapping.as_ref(),
                    &display_id,
                    &name,
                    Event::ToolFailed { call_id, error },
                )
                .await;
                return;
            }

            let event = match tokio::time::timeout(timeout, result_rx).await {
                Ok(Ok(result)) => result_to_event(call_id, result),
                // Sender dropped: `RemoteToolRouter::disconnect` drained us.
                Ok(Err(_)) => Event::ToolFailed {
                    call_id,
                    error: format!("companion disconnected while tool '{name}' was running"),
                },
                Err(_) => {
                    pending
                        .lock()
                        .expect("remote tool pending map poisoned")
                        .remove(&call_id);
                    Event::ToolFailed {
                        call_id,
                        error: format!(
                            "remote tool '{name}' timed out after {}s",
                            timeout.as_secs()
                        ),
                    }
                }
            };
            report_remote_result(
                &sink,
                &obs,
                store.as_ref(),
                mapping.as_ref(),
                &display_id,
                &name,
                event,
            )
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;
    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime, ToolCallId, ToolCapability,
    };
    use sven_wire::{TetherMessage, ToolCallResult, WireToolOutput, WireToolOutputPart};
    use tokio::sync::mpsc;

    use super::{RemoteToolExecutor, RemoteToolRouter};

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
        Top,
        Idle,
        Done,
    }

    /// Records the first domain event (kind + payload debug) then terminates.
    struct OneShotMachine(MachineId);
    impl OneShotMachine {
        fn new() -> Self {
            Self(MachineId::new())
        }
    }

    impl sven_hsm::Machine for OneShotMachine {
        type State = TS;
        fn id(&self) -> MachineId {
            self.0
        }
        fn top(&self) -> TS {
            TS::Top
        }
        fn initial(&self) -> TS {
            TS::Idle
        }
        fn superstate(&self, s: TS) -> TS {
            match s {
                TS::Top => TS::Top,
                _ => TS::Top,
            }
        }
        fn is_terminal(&self, s: TS) -> bool {
            s == TS::Done
        }
        fn dispatch_state(&mut self, s: TS, e: &Event, ctx: &mut Context) -> Reaction<TS> {
            match s {
                TS::Top => Reaction::Handled(vec![]),
                TS::Idle => {
                    if e.is_lifecycle() {
                        return Reaction::Handled(vec![]);
                    }
                    ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                    ctx.set_fact("received_event", format!("{e:?}"));
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got domain event".into(),
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
    }

    fn call_tool_effect(call_id: ToolCallId) -> Effect {
        Effect::CallTool {
            call_id,
            name: "shell".into(),
            capability: ToolCapability::ExecuteShell,
            args: json!({ "command": "ls" }),
        }
    }

    /// Runs `effect` through `exec` against a one-shot kernel and returns
    /// `(event_kind, event_debug)` of the first domain event the machine saw.
    async fn run_remote_effect(exec: &mut RemoteToolExecutor, effect: Effect) -> (String, String) {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &ObservationSink::default())
            .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        let fact = |key: &str| {
            report
                .ctx
                .fact(key)
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "no event received".into())
        };
        (fact("received_event_kind"), fact("received_event"))
    }

    /// A fake companion: answers every `ToolCallRequest` frame via `reply`.
    fn spawn_fake_companion(
        mut rx: mpsc::Receiver<TetherMessage>,
        router: RemoteToolRouter,
        reply: impl Fn(sven_wire::ToolCallRequest) -> ToolCallResult + Send + 'static,
    ) {
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let TetherMessage::ToolCallRequest(req) = msg {
                    router.deliver(reply(req));
                }
            }
        });
    }

    #[tokio::test]
    async fn successful_remote_call_emits_tool_succeeded() {
        let (tx, rx) = mpsc::channel(8);
        let mut exec = RemoteToolExecutor::new(tx);
        spawn_fake_companion(rx, exec.router(), |req| {
            assert_eq!(req.name, "shell");
            assert_eq!(req.args, json!({ "command": "ls" }));
            ToolCallResult::success(
                req.call_id,
                WireToolOutput {
                    content: "file-a\nfile-b".into(),
                    parts: vec![WireToolOutputPart::Text("file-a\nfile-b".into())],
                    is_error: false,
                },
            )
        });

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolSucceeded");
        assert!(event.contains("file-a"), "observation missing: {event}");
        assert_eq!(exec.router().pending_calls(), 0, "pending map must drain");
    }

    #[tokio::test]
    async fn disallowed_capability_is_denied_and_not_forwarded() {
        // A restricted allowed-capability set must reject a CallTool whose
        // capability is not in the set *before* forwarding it to the companion,
        // exactly like `ToolExecutor` does locally.
        let (tx, rx) = mpsc::channel(8);
        let seen = Arc::new(std::sync::Mutex::new(0usize));
        let seen_reply = Arc::clone(&seen);
        let mut exec = RemoteToolExecutor::new(tx)
            .with_allowed_capabilities([ToolCapability::ReadFile].into_iter().collect());
        spawn_fake_companion(rx, exec.router(), move |req| {
            *seen_reply.lock().unwrap() += 1;
            ToolCallResult::success(
                req.call_id,
                WireToolOutput {
                    content: "ran".into(),
                    parts: vec![],
                    is_error: false,
                },
            )
        });

        // call_tool_effect uses ExecuteShell, which is NOT in the allowed set.
        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(
            event.contains("not permitted"),
            "expected capability denial: {event}"
        );
        assert_eq!(
            *seen.lock().unwrap(),
            0,
            "a disallowed-capability call must never reach the companion"
        );
        assert_eq!(exec.router().pending_calls(), 0, "no waiter must be left");
    }

    #[tokio::test]
    async fn tool_level_error_output_emits_tool_failed() {
        let (tx, rx) = mpsc::channel(8);
        let mut exec = RemoteToolExecutor::new(tx);
        spawn_fake_companion(rx, exec.router(), |req| {
            ToolCallResult::success(
                req.call_id,
                WireToolOutput {
                    content: "ls: cannot access".into(),
                    parts: vec![],
                    is_error: true,
                },
            )
        });

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(event.contains("cannot access"), "error missing: {event}");
    }

    #[tokio::test]
    async fn dispatch_failure_emits_tool_failed() {
        let (tx, rx) = mpsc::channel(8);
        let mut exec = RemoteToolExecutor::new(tx);
        spawn_fake_companion(rx, exec.router(), |req| {
            ToolCallResult::failure(req.call_id, "unknown tool 'shell'")
        });

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(event.contains("unknown tool"), "error missing: {event}");
    }

    #[tokio::test]
    async fn timeout_emits_tool_failed_and_drains_pending() {
        let (tx, _rx) = mpsc::channel(8);
        // Keep _rx alive so the send succeeds, but never answer.
        let mut exec = RemoteToolExecutor::new(tx).with_timeout(Duration::from_millis(50));

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(
            event.contains("timed out"),
            "expected timeout error: {event}"
        );
        assert_eq!(
            exec.router().pending_calls(),
            0,
            "timed-out call must be removed"
        );
    }

    #[tokio::test]
    async fn companion_disconnect_fails_in_flight_call() {
        let (tx, mut rx) = mpsc::channel(8);
        // Timeout far in the future: the failure must come from disconnect.
        let mut exec = RemoteToolExecutor::new(tx).with_timeout(Duration::from_secs(60));
        let router = exec.router();
        tokio::spawn(async move {
            // Receive the request, then drop the connection without answering.
            let _ = rx.recv().await;
            router.disconnect();
        });

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(
            event.contains("disconnected"),
            "expected disconnect error: {event}"
        );
    }

    #[tokio::test]
    async fn closed_outbound_channel_emits_tool_failed() {
        let (tx, rx) = mpsc::channel(8);
        drop(rx); // tether writer already gone
        let mut exec = RemoteToolExecutor::new(tx);

        let (kind, event) = run_remote_effect(&mut exec, call_tool_effect(ToolCallId::new())).await;
        assert_eq!(kind, "ToolFailed");
        assert!(
            event.contains("disconnected"),
            "expected disconnect error: {event}"
        );
        assert_eq!(exec.router().pending_calls(), 0);
    }

    #[tokio::test]
    async fn result_for_unknown_call_id_is_ignored() {
        let router = RemoteToolRouter::default();
        let delivered = router.deliver(ToolCallResult::failure(ToolCallId::new(), "late"));
        assert!(!delivered, "unknown call_id must not match a waiter");
    }

    /// Records every effect it receives (delegate stand-in).
    struct RecordingExecutor {
        effects: Arc<std::sync::Mutex<Vec<Effect>>>,
    }

    #[async_trait::async_trait]
    impl EffectExecutor for RecordingExecutor {
        async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
            self.effects.lock().unwrap().push(effect);
        }
    }

    #[tokio::test]
    async fn non_call_tool_effects_are_delegated() {
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (tx, _rx) = mpsc::channel(8);
        let mut exec = RemoteToolExecutor::new(tx).with_delegate(Box::new(RecordingExecutor {
            effects: Arc::clone(&recorded),
        }));

        let effect = Effect::EmitInternal {
            name: "tick".into(),
            payload: json!({}),
        };
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &ObservationSink::default())
            .await;
        rt.abort();

        let effects = recorded.lock().unwrap();
        assert_eq!(effects.len(), 1, "delegate must receive the effect");
        assert!(matches!(&effects[0], Effect::EmitInternal { name, .. } if name == "tick"));
    }
}
