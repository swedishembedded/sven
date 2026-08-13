//! Internal re-emit effect executor.
//!
//! Handles [`Effect::EmitInternal`]: immediately posts the named internal
//! signal back into the kernel queue as
//! `Event::Internal(InternalEvent::Custom { name, payload })`.
//!
//! This is used by machines that need to self-schedule a domain-internal
//! signal without leaving the pure transition model (e.g. completing a
//! sub-phase and immediately re-dispatching the result within the same
//! machine without waiting for external input).

use async_trait::async_trait;
use sven_hsm::event::InternalEvent;
use sven_hsm::{Effect, Event, ObservationSink};
use sven_kernel::{EffectExecutor, EventSink};

/// Executes [`Effect::EmitInternal`] by re-posting the signal to the kernel queue.
pub struct InternalExecutor;

impl InternalExecutor {
    /// Creates a new internal re-emit executor.
    pub fn new() -> Self {
        Self
    }
}

impl Default for InternalExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EffectExecutor for InternalExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let Effect::EmitInternal { name, payload } = effect else {
            return;
        };

        tracing::debug!(name = %name, "InternalExecutor: re-emitting internal signal");
        let _ = sink
            .emit(Event::Internal(InternalEvent::Custom { name, payload }))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_hsm::{
        Context, Effect, Event, Hsm, MachineId, ObservationSink, PermissionPolicy, Reaction,
    };
    use sven_kernel::{EffectExecutor, EventSink, Runtime};

    use super::InternalExecutor;

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
                        return Reaction::Handled(vec![]);
                    }
                    ctx.set_fact("received_event_kind", format!("{:?}", e.kind()));
                    Reaction::Transition {
                        target: TS::Done,
                        effects: vec![],
                        rationale: "got event".into(),
                    }
                }
                TS::Done => Reaction::Handled(vec![]),
            }
        }
    }

    struct NoOpExec;
    #[async_trait::async_trait]
    impl EffectExecutor for NoOpExec {
        async fn execute(&mut self, _: Effect, _: &EventSink, _: &ObservationSink) {}
    }

    async fn run_internal_effect(exec: &mut InternalExecutor, effect: Effect) -> String {
        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;
        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        report
            .ctx
            .fact("received_event_kind")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "no event received".into())
    }

    #[tokio::test]
    async fn emit_internal_posts_custom_event() {
        let mut exec = InternalExecutor::new();
        let effect = Effect::EmitInternal {
            name: "my_signal".into(),
            payload: json!({"data": 42}),
        };
        let kind = run_internal_effect(&mut exec, effect).await;
        assert_eq!(kind, "Custom");
    }

    #[tokio::test]
    async fn non_emit_internal_effect_is_ignored() {
        let mut exec = InternalExecutor::new();
        // PersistAudit should be silently ignored.
        let effect = Effect::PersistAudit;

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            NoOpExec,
            16,
        );
        let sink = rt.sink();
        exec.execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;

        // The machine should NOT have transitioned since no event was emitted.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!rt.status().done, "machine should still be in Idle");
        rt.abort();
    }
}
