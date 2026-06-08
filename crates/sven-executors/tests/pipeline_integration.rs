//! End-to-end kernel pipeline integration tests.
//!
//! These tests verify the complete path from a user message through the HSM
//! kernel, through the executor, and out to the observation plane as `UiEvent`s.
//!
//! They use only the in-process mock model so no network access is needed.
//! The goal is to prove that text deltas and `TurnComplete` reach the
//! observation sink correctly.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{
    Context, Effect, EffectExecutor, ErasedRuntime, Event, EventSink, Hsm, MachineId,
    ObservationSink, PermissionPolicy, Reaction, UiEvent,
};
use tokio::sync::Mutex;

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// Minimal one-shot machine that transitions to a terminal state on the first
/// non-lifecycle event, recording the event kind as a context fact.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum OneShotState {
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
    type State = OneShotState;
    fn id(&self) -> MachineId {
        self.0
    }
    fn top(&self) -> OneShotState {
        OneShotState::Top
    }
    fn initial(&self) -> OneShotState {
        OneShotState::Idle
    }
    fn superstate(&self, _s: OneShotState) -> OneShotState {
        OneShotState::Top
    }
    fn is_terminal(&self, s: OneShotState) -> bool {
        s == OneShotState::Done
    }
    fn dispatch_state(
        &mut self,
        s: OneShotState,
        e: &Event,
        ctx: &mut sven_hsm::Context,
    ) -> Reaction<OneShotState> {
        match s {
            OneShotState::Idle if !e.is_lifecycle() => {
                ctx.set_fact("received_kind", format!("{:?}", e.kind()));
                Reaction::Transition {
                    target: OneShotState::Done,
                    effects: vec![],
                    rationale: "got domain event".into(),
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

/// Drive `executor.execute(effect, &sink, &obs)` against a fresh one-shot
/// runtime and collect all `UiEvent`s that arrived on the observation sink.
async fn drive_effect(
    executor: &mut impl EffectExecutor,
    effect: Effect,
) -> (Vec<UiEvent>, String) {
    let obs = ObservationSink::new(128);
    let mut obs_rx = obs.subscribe();

    let rt = sven_hsm::Runtime::spawn(
        Hsm::new(OneShotMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        NoOpExec,
        16,
    );
    let sink = rt.sink();
    executor.execute(effect, &sink, &obs).await;
    rt.wait_done().await;
    let report = rt.join().await.unwrap();

    let mut events = Vec::new();
    while let Ok(ev) = obs_rx.try_recv() {
        events.push(ev);
    }
    let received_kind = report
        .ctx
        .fact("received_kind")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "no event received".into());
    (events, received_kind)
}

// ─────────────────────────────────────────────────────────────────────────────
// PongProvider: mock ModelProvider for tests
// ─────────────────────────────────────────────────────────────────────────────

/// Streams "pong" then Done — used to test the TurnExecutor path.
struct PongProvider;

#[async_trait]
impl sven_model::ModelProvider for PongProvider {
    fn name(&self) -> &str {
        "pong"
    }
    fn model_name(&self) -> &str {
        "pong"
    }
    async fn complete(
        &self,
        _req: sven_model::CompletionRequest,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = anyhow::Result<sven_model::ResponseEvent>> + Send>,
        >,
    > {
        let events: Vec<anyhow::Result<sven_model::ResponseEvent>> = vec![
            Ok(sven_model::ResponseEvent::TextDelta("pong".into())),
            Ok(sven_model::ResponseEvent::Done),
        ];
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// CompositeExecutor + ReactiveAgentMachine full-kernel integration test
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reactive_agent_machine_routes_user_message_to_text_delta_on_obs_sink() {
    use sven_core::ReactiveAgentMachine;
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

    // Build a TurnExecutor backed by PongProvider so the machine's
    // kind="turn" CallLlm effect is handled end-to-end without a real LLM.
    let store = Arc::new(std::sync::Mutex::new(sven_llm::ConversationStore::new()));
    let call_id_to_thread = Arc::new(std::sync::Mutex::new(
        std::collections::HashMap::<sven_hsm::ToolCallId, String>::new(),
    ));
    let cancel_handle = Arc::new(Mutex::new(None));
    let turn_exec = sven_executors::TurnExecutor::new(
        Arc::new(PongProvider),
        None,
        Arc::new(sven_tools::ToolRegistry::new()),
        store,
        call_id_to_thread,
        cancel_handle,
    );
    let executor = CompositeExecutorBuilder::default()
        .with_turn(turn_exec)
        .build();

    // The ErasedRuntime requires a `Box<dyn ErasedMachine>`, which is
    // `Hsm<M>` wrapped in a Box — exactly what the mode registry does.
    let machine: Box<dyn ErasedMachine> = Box::new(Hsm::new(ReactiveAgentMachine::new()));
    let rt = ErasedRuntime::spawn(
        machine,
        Context::new(),
        PermissionPolicy::builder().build(),
        executor,
        64,
    );

    // Subscribe to the runtime's own outward observation plane.
    let mut rt_obs_rx = rt.subscribe_observations();

    // Send the user message directly into the kernel queue.
    let sent = rt.sink().emit(Event::UserMessage { text: "ping".into() }).await;
    assert!(sent, "UserMessage must be accepted by the kernel sink");

    // Collect observations with a bounded wait (500 ms is enough for in-process mock).
    let mut events: Vec<UiEvent> = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(500);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            Ok(ev) = rt_obs_rx.recv() => {
                let is_turn_complete = ev == UiEvent::TurnComplete;
                events.push(ev);
                if is_turn_complete { break; }
            }
            _ = tokio::time::sleep(remaining) => break,
        }
    }

    assert!(
        events.contains(&UiEvent::TextDelta("pong".into())),
        "TextDelta('pong') must arrive via ReactiveAgentMachine → TurnExecutor path: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive after a full user turn: {events:?}"
    );

    // Clean up
    rt.abort();
}
