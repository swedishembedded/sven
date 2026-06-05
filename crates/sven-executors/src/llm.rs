//! LLM effect executor.
//!
//! Handles [`Effect::CallLlm`]: deserialises the opaque [`serde_json::Value`]
//! payload as a [`LlmRequest`], calls the injected [`LlmAdapter`], and emits
//! the resulting [`Event`] (or `Event::LlmFailed` on error) to the kernel
//! queue.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Effect, EffectExecutor, Event, EventSink, ObservationSink};
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
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
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
                return;
            }
        };

        let kind = req.kind_name();
        match self.adapter.invoke(req).await {
            Ok(event) => {
                let _ = sink.emit(event).await;
            }
            Err(e) => {
                tracing::warn!(request_kind = kind, error = %e, "LLM adapter error");
                let _ = sink
                    .emit(Event::LlmFailed {
                        error: e.to_string(),
                    })
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use sven_hsm::{
        Context, Effect, EffectExecutor, Event, EventSink, Hsm, MachineId, ObservationSink,
        PermissionPolicy, Reaction, Runtime,
    };
    use sven_llm::{LlmRequest, MockLlmAdapter};

    use super::LlmExecutor;

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
        executor.execute(effect, &sink, &sven_hsm::ObservationSink::default()).await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report
            .ctx
            .fact("received_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "no event received".into())
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
