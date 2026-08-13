//! Runtime (Active Object) tests: deterministic virtual-time timeouts, the
//! permission gate rejecting effects before execution, and the
//! executor-result-re-enters-the-queue loop.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{AgentMachine, GuardMachine, TState, TimerMachine};
use sven_hsm::{Context, Effect, Event, Hsm, ObservationSink, PermissionPolicy};
use sven_kernel::{Clock, EffectExecutor, EventSink, Runtime, TimerService, VirtualClock};

/// Executor that turns `ScheduleTimeout`/`CancelTimeout` into real timer tasks
/// via a [`TimerService`] backed by the injected clock.
struct TimerExec {
    clock: Arc<dyn Clock>,
    timer: Option<TimerService>,
}

#[async_trait]
impl EffectExecutor for TimerExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let timer = self
            .timer
            .get_or_insert_with(|| TimerService::new(Arc::clone(&self.clock), sink.clone()));
        match effect {
            Effect::ScheduleTimeout { timer_id, duration } => timer.schedule(timer_id, duration),
            Effect::CancelTimeout { timer_id } => timer.cancel(timer_id),
            _ => {}
        }
    }
}

#[tokio::test]
async fn virtual_time_timeout_drives_machine_to_terminal() {
    let clock = VirtualClock::new();
    let exec = TimerExec {
        clock: Arc::new(clock.clone()),
        timer: None,
    };

    let rt = Runtime::spawn(
        Hsm::new(TimerMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // Kick the machine into Waiting; its entry schedules a 30s timeout.
    assert!(rt.post(Event::user_message("start")).await);
    rt.wait_for_state("Waiting").await;
    assert_eq!(rt.status().state_label, "Waiting");
    assert!(!rt.status().done, "must not fire before time advances");

    // Advance virtual time past the deadline; the timer fires deterministically.
    clock.advance(Duration::from_secs(31));
    rt.wait_done().await;

    let report = rt.join().await.expect("runtime task joined");
    assert_eq!(report.hsm.state(), TState::Fired);
}

/// No-op executor that simply records the effects it was handed.
struct RecordingExec {
    seen: Arc<Mutex<Vec<sven_hsm::EffectKind>>>,
}

#[async_trait]
impl EffectExecutor for RecordingExec {
    async fn execute(&mut self, effect: Effect, _sink: &EventSink, _obs: &ObservationSink) {
        self.seen.lock().unwrap().push(effect.kind());
    }
}

#[tokio::test]
async fn runtime_rejects_forbidden_effects_before_executing_them() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let exec = RecordingExec {
        seen: Arc::clone(&seen),
    };

    // Empty policy: the GuardMachine's entry CallTool(ExecuteShell) is forbidden.
    let rt = Runtime::spawn(
        Hsm::new(GuardMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // Wait until the initial dispatch has been processed and published.
    let mut status = rt.status_watch();
    let _ = status.wait_for(|s| s.last_error.is_some()).await;

    let st = rt.status();
    assert!(
        st.last_error.is_some(),
        "the forbidden effect must produce a permission error"
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "no effect may execute when the batch is rejected"
    );

    // The audit trail records the rejection.
    let audit = rt.audit_snapshot();
    assert!(
        audit
            .iter()
            .any(|r| matches!(r.outcome, sven_hsm::AuditOutcome::Rejected)),
        "a Rejected audit record must be present"
    );

    rt.abort();
}

/// Executor that answers each `CallLlm` by posting a typed `LlmProposedResponse`
/// back into the queue - exercising the "results re-enter as events" loop.
struct LlmExec {
    calls: Arc<Mutex<u32>>,
}

#[async_trait]
impl EffectExecutor for LlmExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        if let Effect::CallLlm { .. } = effect {
            *self.calls.lock().unwrap() += 1;
            sink.emit(Event::LlmProposedResponse {
                text: "here you go".into(),
            })
            .await;
        }
    }
}

#[tokio::test]
async fn executor_results_re_enter_the_queue_as_events() {
    let calls = Arc::new(Mutex::new(0));
    let exec = LlmExec {
        calls: Arc::clone(&calls),
    };

    let rt = Runtime::spawn(
        Hsm::new(AgentMachine::new()),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );

    // UserMessage -> Drafting (emits CallLlm). The executor posts
    // LlmProposedResponse, which the kernel dispatches: Drafting -> Listening.
    assert!(rt.post(Event::user_message("hi")).await);

    // Two events get processed: the UserMessage and the re-entered response.
    let mut status = rt.status_watch();
    let _ = status.wait_for(|s| s.processed >= 2).await;

    assert_eq!(rt.status().state_label, "Listening");
    assert_eq!(*calls.lock().unwrap(), 1, "the LLM was called exactly once");

    rt.abort();
}
