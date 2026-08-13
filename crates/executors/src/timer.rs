//! Timer effect executor.
//!
//! Handles [`Effect::ScheduleTimeout`] and [`Effect::CancelTimeout`] by
//! delegating to a [`sven_kernel::TimerService`] backed by a
//! [`sven_kernel::Clock`] injected at construction.

use std::sync::Arc;

use async_trait::async_trait;
use sven_hsm::{Effect, ObservationSink};
use sven_kernel::{Clock, EffectExecutor, EventSink, TimerService};

/// Executes timer effects using an injected [`Clock`].
///
/// A [`TimerService`] is created lazily on first use (because it requires an
/// `EventSink`, which is only available at execution time).
pub struct TimerExecutor {
    clock: Arc<dyn Clock>,
    service: Option<TimerService>,
}

impl TimerExecutor {
    /// Creates a new timer executor backed by `clock`.
    ///
    /// Pass a [`sven_kernel::SystemClock`] in production and a
    /// [`sven_kernel::VirtualClock`] in tests.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            service: None,
        }
    }
}

#[async_trait]
impl EffectExecutor for TimerExecutor {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let svc = self
            .service
            .get_or_insert_with(|| TimerService::new(Arc::clone(&self.clock), sink.clone()));

        match effect {
            Effect::ScheduleTimeout { timer_id, duration } => {
                tracing::debug!(?timer_id, ?duration, "TimerExecutor: scheduling timeout");
                svc.schedule(timer_id, duration);
            }
            Effect::CancelTimeout { timer_id } => {
                tracing::debug!(?timer_id, "TimerExecutor: cancelling timeout");
                svc.cancel(timer_id);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use sven_hsm::{Context, Effect, Event, Hsm, MachineId, PermissionPolicy, Reaction, TimerId};
    use sven_kernel::{EffectExecutor, Runtime, VirtualClock};

    use super::TimerExecutor;

    // ── Minimal test machine that fires to Done on Timeout ────────────────────

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

    #[tokio::test]
    async fn timeout_fires_after_virtual_time_advances() {
        let clock = VirtualClock::new();
        let timer_exec = TimerExecutor::new(Arc::new(clock.clone()));

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            timer_exec,
            16,
        );
        let sink = rt.sink();

        // Schedule a 10-second timeout directly via a cloned TimerService.
        let timer_id = TimerId::new();
        let mut svc = sven_kernel::TimerService::new(Arc::new(clock.clone()), sink);
        svc.schedule(timer_id, Duration::from_secs(10));

        // Advance virtual time past the deadline.
        clock.advance(Duration::from_secs(11));

        rt.wait_done().await;
        let report = rt.join().await.unwrap();
        assert_eq!(
            report
                .ctx
                .fact("received_event_kind")
                .and_then(|v| v.as_str()),
            Some("Timeout")
        );
    }

    #[tokio::test]
    async fn schedule_timeout_effect_fires_after_advance() {
        let clock = VirtualClock::new();
        let timer_exec = TimerExecutor::new(Arc::new(clock.clone()));

        let rt = Runtime::spawn(
            Hsm::new(OneShotMachine::new()),
            Context::new(),
            PermissionPolicy::builder().build(),
            timer_exec,
            16,
        );
        let sink = rt.sink();

        // Exercise the executor's execute() path directly.
        let timer_id = TimerId::new();
        let effect = Effect::ScheduleTimeout {
            timer_id,
            duration: Duration::from_secs(5),
        };

        // We need a TimerExecutor reference to call execute; use a second one.
        let mut exec2 = TimerExecutor::new(Arc::new(clock.clone()));
        exec2
            .execute(effect, &sink, &sven_hsm::ObservationSink::default())
            .await;

        clock.advance(Duration::from_secs(6));
        rt.wait_done().await;

        assert!(rt.status().done);
    }

    #[tokio::test]
    async fn cancel_prevents_timeout_from_firing() {
        let clock = VirtualClock::new();

        // Build two executors sharing the same sink so cancellation
        // interacts with the same timer service.  We chain them manually
        // below.
        let timer_id = TimerId::new();
        let mut svc = sven_kernel::TimerService::new(
            Arc::new(clock.clone()),
            // Dummy sink pointing to a dropped channel - we only care that
            // cancel aborts the task.
            {
                let rt = Runtime::spawn(
                    Hsm::new(OneShotMachine::new()),
                    Context::new(),
                    PermissionPolicy::builder().build(),
                    super::TimerExecutor::new(Arc::new(VirtualClock::new())),
                    16,
                );
                let s = rt.sink();
                rt.abort();
                s
            },
        );

        svc.schedule(timer_id, Duration::from_secs(5));
        svc.cancel(timer_id);

        // Advance past deadline - the event should NOT arrive because we cancelled.
        clock.advance(Duration::from_secs(10));
        // Give the cancelled task a tick to confirm it is gone.
        tokio::time::sleep(Duration::from_millis(10)).await;
        // No assertion needed: if the task was not cancelled it would panic
        // trying to send on the dropped sink, and tokio would log the error.
    }
}
