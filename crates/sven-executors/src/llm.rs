//! LLM effect executor.
//!
//! Handles [`Effect::CallLlm`]: deserialises the opaque [`serde_json::Value`]
//! payload as a [`LlmRequest`], calls the injected [`LlmAdapter`], and emits
//! the resulting [`Event`] (or `Event::LlmFailed` on error) to the kernel
//! queue.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink, UiEvent};
use sven_llm::{LlmAdapter, LlmRequest};

/// Executes [`Effect::CallLlm`] by driving the injected [`LlmAdapter`].
pub struct LlmExecutor {
    adapter: Arc<dyn LlmAdapter>,
}

impl LlmExecutor {
    /// Creates an executor backed by `adapter`.
    pub fn new(adapter: Arc<dyn LlmAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait]
impl EffectExecutor for LlmExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, obs: &ObservationSink) {
        let Effect::CallLlm { request } = effect else {
            return;
        };

        let req = match LlmRequest::from_value(request) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "failed to deserialise LlmRequest");
                let _ = sink
                    .emit(Event::LlmFailed {
                        error: format!("failed to deserialise LlmRequest: {e}"),
                    })
                    .await;
                // Ensure the TUI spinner stops even on deserialization failure.
                obs.emit(UiEvent::TurnComplete);
                return;
            }
        };

        let kind = req.kind_name();
        // Pass `obs` so the adapter can forward streaming text/thinking deltas
        // and usage events to the UI in real time.
        match self.adapter.invoke(req, Some(obs)).await {
            Ok(event) => {
                let _ = sink.emit(event).await;
            }
            Err(e) => {
                let error_msg = e.to_string();
                tracing::warn!(request_kind = kind, error = %error_msg, "LLM adapter error");
                obs.emit(UiEvent::Error(error_msg.clone()));
                let _ = sink
                    .emit(Event::LlmFailed { error: error_msg })
                    .await;
            }
        }
        // Signal to the TUI that this LLM turn has completed so the spinner stops.
        obs.emit(UiEvent::TurnComplete);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime, UiEvent,
    };
    use sven_llm::{LlmAdapter, LlmError, LlmRequest, MockLlmAdapter};

    use super::LlmExecutor;

    // ── Streaming mock adapter ─────────────────────────────────────────────────
    //
    // Unlike MockLlmAdapter (which returns a pre-programmed event without
    // emitting any observations), StreamingMockAdapter emits UiEvent::TextDelta
    // events onto the caller-supplied obs sink before returning the final event.
    // This lets us verify that LlmExecutor correctly threads the obs through.

    struct StreamingMockAdapter {
        deltas: Vec<String>,
        result_event: Event,
    }

    impl StreamingMockAdapter {
        fn new(deltas: Vec<&str>, event: Event) -> Self {
            Self {
                deltas: deltas.into_iter().map(str::to_string).collect(),
                result_event: event,
            }
        }
    }

    #[async_trait]
    impl LlmAdapter for StreamingMockAdapter {
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
            Ok(self.result_event.clone())
        }
    }

    // ── Minimal one-shot test machine ─────────────────────────────────────────
    //
    // Starts in Idle. Transitions to Done on the first non-lifecycle event.
    // This lets us verify that the executor emits *an* event without needing
    // direct access to the EventSink's internal channel.

    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    enum TS {
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
                        Reaction::Handled(vec![])
                    } else {
                        ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                        Reaction::Transition {
                            target: TS::Done,
                            effects: vec![],
                            rationale: "got domain event".into(),
                        }
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    /// No-op executor used as a stand-in where the real executor is injected
    /// externally via the `EventSink`.
    struct NoOpExec;

    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {}
    }

    /// Runs the executor against a single effect using a one-shot Runtime,
    /// returning the fact stored by the recording machine.
    async fn run_executor_effect(executor: &mut impl EffectExecutor, effect: Effect) -> String {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        executor
            .execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report
            .ctx
            .fact("received_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "no event received".into())
    }

    // ── Observation-forwarding tests ───────────────────────────────────────────

    #[tokio::test]
    async fn forwards_text_deltas_to_observation_sink() {
        let adapter = Arc::new(StreamingMockAdapter::new(
            vec!["Hello", " world"],
            Event::LlmProposedResponse {
                text: "Hello world".into(),
            },
        ));
        let mut exec = LlmExecutor::new(adapter);
        let obs = ObservationSink::new(64);
        let mut obs_rx = obs.subscribe();

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        let req = LlmRequest::GenerateResponse {
            intent: json!({"intent": "greeting"}),
        };
        let effect = Effect::CallLlm {
            request: req.to_value(),
        };
        exec.execute(effect, &sink, &obs).await;
        rt.wait_done().await;
        rt.join().await.unwrap();

        let mut events: Vec<UiEvent> = Vec::new();
        while let Ok(ev) = obs_rx.try_recv() {
            events.push(ev);
        }

        assert!(
            events.contains(&UiEvent::TextDelta("Hello".into())),
            "expected TextDelta('Hello') but got: {events:?}"
        );
        assert!(
            events.contains(&UiEvent::TextDelta(" world".into())),
            "expected TextDelta(' world') but got: {events:?}"
        );
        assert!(
            events.contains(&UiEvent::TextComplete("Hello world".into())),
            "expected TextComplete but got: {events:?}"
        );
        assert!(
            events.contains(&UiEvent::TurnComplete),
            "expected TurnComplete but got: {events:?}"
        );
    }

    #[tokio::test]
    async fn emits_turn_complete_even_on_deserialization_failure() {
        let adapter = Arc::new(MockLlmAdapter::empty());
        let mut exec = LlmExecutor::new(adapter);
        let obs = ObservationSink::new(16);
        let mut obs_rx = obs.subscribe();

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        let effect = Effect::CallLlm {
            request: json!({"kind": "totally_unknown_variant_xyz"}),
        };
        exec.execute(effect, &sink, &obs).await;
        rt.wait_done().await;
        rt.join().await.unwrap();

        let mut events: Vec<UiEvent> = Vec::new();
        while let Ok(ev) = obs_rx.try_recv() {
            events.push(ev);
        }
        assert!(
            events.contains(&UiEvent::TurnComplete),
            "TurnComplete must be emitted even on deserialization failure: {events:?}"
        );
    }

    #[tokio::test]
    async fn emits_llm_proposed_assessment_on_success() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![Event::LlmProposedAssessment {
            assessment: json!({"intent": "bugfix", "confidence": 0.9}),
        }]));
        let mut exec = LlmExecutor::new(adapter);

        let req = LlmRequest::ExtractIntent {
            text: "fix the crash".into(),
            allowed_intents: vec!["bugfix".into()],
        };
        let effect = Effect::CallLlm {
            request: req.to_value(),
        };
        let kind = run_executor_effect(&mut exec, effect).await;
        assert_eq!(kind, "LlmProposedAssessment");
    }

    #[tokio::test]
    async fn emits_llm_failed_on_bad_payload() {
        let adapter = Arc::new(MockLlmAdapter::empty());
        let mut exec = LlmExecutor::new(adapter);

        let effect = Effect::CallLlm {
            request: json!({"kind": "unknown_variant_xyz"}),
        };
        let kind = run_executor_effect(&mut exec, effect).await;
        assert_eq!(kind, "LlmFailed");
    }

    #[tokio::test]
    async fn emits_llm_proposed_plan_for_plan_request() {
        let adapter = Arc::new(MockLlmAdapter::new(vec![Event::LlmProposedPlan {
            plan: json!({"title": "Plan A", "steps": ["step 1"], "risk": "low"}),
        }]));
        let mut exec = LlmExecutor::new(adapter);

        let req = LlmRequest::GenerateCandidatePlan {
            known_context: json!({}),
            planning_policy: "safe".into(),
        };
        let effect = Effect::CallLlm {
            request: req.to_value(),
        };
        let kind = run_executor_effect(&mut exec, effect).await;
        assert_eq!(kind, "LlmProposedPlan");
    }
}
