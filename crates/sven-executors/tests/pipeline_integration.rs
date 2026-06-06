//! End-to-end kernel pipeline integration tests.
//!
//! These tests verify the complete path from a user message through the HSM
//! kernel, through the executor, and out to the observation plane as `UiEvent`s.
//!
//! They use only the in-process mock model/adapter so no network access is
//! needed.  The goal is to prove that text deltas and `TurnComplete` reach the
//! observation sink correctly — which is the root cause of the "no response
//! visible in TUI" bug.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use sven_hsm::{
    Context, Effect, EffectExecutor, ErasedRuntime, Event, EventSink, Hsm, MachineId,
    ObservationSink, PermissionPolicy, Reaction, UiEvent,
};
use sven_llm::{LlmAdapter, LlmError, LlmRequest, MockLlmAdapter};
use tokio::sync::Mutex;

// ── Shared helpers ─────────────────────────────────────────────────────────────

/// A streaming-mock LLM adapter that emits TextDelta observations before
/// returning the pre-programmed event.
struct StreamingMock {
    deltas: Vec<String>,
    event: Event,
}

impl StreamingMock {
    fn new(deltas: Vec<&str>, event: Event) -> Self {
        Self {
            deltas: deltas.iter().map(|s| s.to_string()).collect(),
            event,
        }
    }
}

#[async_trait]
impl LlmAdapter for StreamingMock {
    async fn invoke(
        &self,
        _req: LlmRequest,
        obs: Option<&ObservationSink>,
    ) -> Result<Event, LlmError> {
        if let Some(o) = obs {
            for d in &self.deltas {
                o.emit(UiEvent::TextDelta(d.clone()));
            }
            let full = self.deltas.join("");
            if !full.is_empty() {
                o.emit(UiEvent::TextComplete(full));
            }
        }
        Ok(self.event.clone())
    }
}

// ── LlmExecutor pipeline ───────────────────────────────────────────────────────

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
        ctx: &mut Context,
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
// LlmExecutor tests
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn llm_executor_forwards_text_deltas_to_obs_sink() {
    use sven_executors::LlmExecutor;

    let adapter = Arc::new(StreamingMock::new(
        vec!["Hello", " world"],
        Event::LlmProposedResponse {
            text: "Hello world".into(),
        },
    ));
    let mut exec = LlmExecutor::new(adapter);

    let req = LlmRequest::GenerateResponse {
        intent: json!({"intent": "greeting"}),
    };
    let (events, kind) = drive_effect(&mut exec, Effect::CallLlm { request: req.to_value() }).await;

    assert_eq!(kind, "LlmProposedResponse", "machine must receive LlmProposedResponse");
    assert!(
        events.contains(&UiEvent::TextDelta("Hello".into())),
        "TextDelta('Hello') missing from observation sink: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TextDelta(" world".into())),
        "TextDelta(' world') missing from observation sink: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TextComplete("Hello world".into())),
        "TextComplete missing from observation sink: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete missing from observation sink: {events:?}"
    );
}

#[tokio::test]
async fn llm_executor_emits_turn_complete_on_deserialization_failure() {
    use sven_executors::LlmExecutor;

    let adapter = Arc::new(MockLlmAdapter::empty());
    let mut exec = LlmExecutor::new(adapter);

    let (events, kind) = drive_effect(
        &mut exec,
        Effect::CallLlm {
            request: json!({"kind": "unknown_variant_xyz"}),
        },
    )
    .await;

    assert_eq!(kind, "LlmFailed");
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive even on deserialization failure: {events:?}"
    );
}

#[tokio::test]
async fn llm_executor_emits_error_and_turn_complete_on_adapter_error() {
    use sven_executors::LlmExecutor;

    // An adapter that always returns an error.
    struct FailAdapter;
    #[async_trait]
    impl LlmAdapter for FailAdapter {
        async fn invoke(
            &self,
            _req: LlmRequest,
            _obs: Option<&ObservationSink>,
        ) -> Result<Event, LlmError> {
            Err(LlmError::ProviderError("injected failure".into()))
        }
    }

    let adapter = Arc::new(FailAdapter);
    let mut exec = LlmExecutor::new(adapter);

    let req = LlmRequest::ExtractIntent {
        text: "anything".into(),
        allowed_intents: vec!["bugfix".into()],
    };
    let (events, kind) =
        drive_effect(&mut exec, Effect::CallLlm { request: req.to_value() }).await;

    assert_eq!(kind, "LlmFailed");
    assert!(
        events.iter().any(|e| matches!(e, UiEvent::Error(_))),
        "UiEvent::Error must be emitted on adapter failure: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive after adapter failure: {events:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// ConverseExecutor pipeline test
// ─────────────────────────────────────────────────────────────────────────────

/// Streams "pong" then Done — used to test the ConverseExecutor path.
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

fn make_agent() -> sven_core::Agent {
    use tokio::sync::mpsc;
    let (_tx, rx) = mpsc::channel(8);
    sven_core::Agent::new_with_params(sven_core::AgentNewParams {
        model: Arc::new(PongProvider),
        tools: Arc::new(sven_tools::ToolRegistry::new()),
        config: Arc::new(sven_config::AgentConfig::default()),
        runtime: sven_core::AgentRuntimeContext::default(),
        mode_lock: Arc::new(Mutex::new(sven_config::AgentMode::Agent)),
        tool_event_rx: rx,
        max_context_tokens: 128_000,
        model_resolver: None,
    })
}

#[tokio::test]
async fn converse_executor_streams_text_deltas_to_obs_sink() {
    use sven_executors::ConverseExecutor;

    let agent = Arc::new(Mutex::new(make_agent()));
    let cancel_handle = Arc::new(Mutex::new(None));
    let mut exec = ConverseExecutor::new(agent, cancel_handle);

    let effect = Effect::CallLlm {
        request: json!({ "kind": "converse", "text": "ping" }),
    };
    let (events, kind) = drive_effect(&mut exec, effect).await;

    assert_eq!(kind, "LlmProposedResponse");
    assert!(
        events.contains(&UiEvent::TextDelta("pong".into())),
        "TextDelta('pong') must arrive on obs sink: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive after converse turn: {events:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CompositeExecutor + ReactiveAgentMachine full-kernel integration test
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn reactive_agent_machine_routes_user_message_to_text_delta_on_obs_sink() {
    use sven_core::ReactiveAgentMachine;
    use sven_executors::CompositeExecutorBuilder;
    use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine};

    let agent = Arc::new(Mutex::new(make_agent()));
    let cancel_handle = Arc::new(Mutex::new(None));

    // Build a composite executor with only the converse sub-executor
    // (no tool registry, user approval, etc. needed for this test).
    let executor = CompositeExecutorBuilder::default()
        .with_converse(agent, cancel_handle)
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
        "TextDelta('pong') must arrive via ReactiveAgentMachine → ConverseExecutor path: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must arrive after a full user turn: {events:?}"
    );

    // Clean up
    rt.abort();
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON suppression tests (Phase 1: is_user_facing gating)
// ─────────────────────────────────────────────────────────────────────────────

/// A mock adapter that always emits text deltas regardless of what the
/// real adapter would do — used to verify gating.
struct AlwaysStreamingAdapter {
    text: String,
    result: Event,
}

#[async_trait]
impl LlmAdapter for AlwaysStreamingAdapter {
    async fn invoke(
        &self,
        _req: LlmRequest,
        obs: Option<&ObservationSink>,
    ) -> Result<Event, LlmError> {
        if let Some(o) = obs {
            o.emit(UiEvent::TextDelta(self.text.clone()));
            o.emit(UiEvent::TextComplete(self.text.clone()));
        }
        Ok(self.result.clone())
    }
}

/// Structured (non-user-facing) requests must NOT emit TextDelta to the UI
/// even when the underlying adapter streams text, because the raw output is
/// JSON that the machine interprets internally.
#[tokio::test]
async fn structured_llm_request_suppresses_text_deltas() {
    use sven_executors::LlmExecutor;

    let adapter = Arc::new(AlwaysStreamingAdapter {
        text: "{\"intent\": \"bug_fix\", \"confidence\": 0.9}".into(),
        result: Event::LlmProposedAssessment {
            assessment: serde_json::json!({"intent": "bug_fix", "confidence": 0.9}),
        },
    });
    let mut exec = LlmExecutor::new(adapter);

    // ExtractIntent is NOT user-facing → text deltas must be suppressed.
    let req = LlmRequest::ExtractIntent {
        text: "fix the null pointer".into(),
        allowed_intents: vec!["bug_fix".into()],
    };
    let (events, _) = drive_effect(&mut exec, Effect::CallLlm { request: req.to_value() }).await;

    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::TextDelta(_))),
        "TextDelta must NOT appear for structured (non-user-facing) requests: {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::TextComplete(_))),
        "TextComplete must NOT appear for structured requests: {events:?}"
    );
    // TurnComplete must still arrive.
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must still be emitted for structured requests: {events:?}"
    );
}

/// User-facing requests (GenerateResponse) MUST emit TextDelta so streaming
/// works in the TUI chat view.
#[tokio::test]
async fn generate_response_streams_text_deltas() {
    use sven_executors::LlmExecutor;

    let response_text = "Hello! How can I help you today?";
    let adapter = Arc::new(AlwaysStreamingAdapter {
        text: response_text.into(),
        result: Event::LlmProposedResponse {
            text: response_text.into(),
        },
    });
    let mut exec = LlmExecutor::new(adapter);

    let req = LlmRequest::GenerateResponse {
        intent: serde_json::json!({"intent": "question"}),
    };
    let (events, _) = drive_effect(&mut exec, Effect::CallLlm { request: req.to_value() }).await;

    assert!(
        events.iter().any(|e| matches!(e, UiEvent::TextDelta(_))),
        "TextDelta must appear for user-facing GenerateResponse: {events:?}"
    );
}

/// EvaluateContext (used by SDLC states) is NOT user-facing and must suppress
/// raw JSON from the observation stream.
#[tokio::test]
async fn evaluate_context_suppresses_json_output() {
    use sven_executors::LlmExecutor;

    let adapter = Arc::new(AlwaysStreamingAdapter {
        text: "{\"summary\": \"baseline built\"}".into(),
        result: Event::LlmProposedAssessment {
            assessment: serde_json::json!({"summary": "baseline built"}),
        },
    });
    let mut exec = LlmExecutor::new(adapter);

    let req = LlmRequest::EvaluateContext {
        goal: "Build a baseline understanding of the codebase.".into(),
        known_context: serde_json::json!({}),
    };
    let (events, _) = drive_effect(&mut exec, Effect::CallLlm { request: req.to_value() }).await;

    assert!(
        !events.iter().any(|e| matches!(e, UiEvent::TextDelta(_))),
        "TextDelta must be suppressed for EvaluateContext: {events:?}"
    );
    assert!(
        events.contains(&UiEvent::TurnComplete),
        "TurnComplete must still arrive: {events:?}"
    );
}
