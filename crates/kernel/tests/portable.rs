// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel behaves the same on every host.
//!
//! Each test here is one body run twice: natively under tokio
//! (`cargo test -p sven-kernel --test portable`) and as wasm32 on a
//! JavaScript event loop with its timers, clock and crypto, the services a
//! browser worker provides (`make test/wasm`, in Node). They assert
//! the exact transition trace, so passing on both is parity by construction,
//! not by comparing two implementations: there is one.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{AgentMachine, TimerMachine};
use sven_hsm::{Context, Effect, Event, Hsm, ObservationSink, PermissionPolicy, UiEvent};
use sven_kernel::{Clock, EffectExecutor, ErasedRuntime, EventSink, SystemClock, TimerService};

/// Answers every model call with a reply, and runs timers on the host's
/// real clock.
struct HostExec {
    clock: Arc<dyn Clock>,
    timer: Option<TimerService>,
    model_calls: Arc<Mutex<u32>>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl EffectExecutor for HostExec {
    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        match effect {
            Effect::CallLlm { .. } => {
                *self.model_calls.lock().unwrap() += 1;
                sink.emit(Event::LlmProposedResponse {
                    text: "here you go".into(),
                })
                .await;
            }
            Effect::ScheduleTimeout { timer_id, duration } => self
                .timer
                .get_or_insert_with(|| TimerService::new(Arc::clone(&self.clock), sink.clone()))
                .schedule(timer_id, duration),
            _ => {}
        }
    }
}

fn host_exec() -> (HostExec, Arc<Mutex<u32>>) {
    let model_calls = Arc::new(Mutex::new(0));
    let exec = HostExec {
        clock: Arc::new(SystemClock::new()),
        timer: None,
        model_calls: Arc::clone(&model_calls),
    };
    (exec, model_calls)
}

/// The transitions a run made, in order, from its observation stream.
fn transitions(rx: &mut tokio::sync::broadcast::Receiver<UiEvent>) -> Vec<String> {
    let mut seen = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let UiEvent::Transition { from, to, event } = event {
            seen.push(format!("{from} -{event}-> {to}"));
        }
    }
    seen
}

async fn a_model_reply_re_enters_the_queue_and_moves_the_machine() {
    let (exec, model_calls) = host_exec();
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(AgentMachine::new())),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );
    let mut observations = rt.subscribe_observations();
    assert!(rt.post(Event::user_message("hi")).await);
    let mut status = rt.status_watch();
    let _ = status.wait_for(|s| s.processed >= 2).await;

    assert_eq!(rt.status().state_label, "Listening");
    assert_eq!(
        *model_calls.lock().unwrap(),
        1,
        "the model was called exactly once"
    );
    assert_eq!(
        transitions(&mut observations),
        vec![
            "Listening -UserMessage-> Drafting",
            "Drafting -LlmProposedResponse-> Listening"
        ],
    );
    rt.abort();
}

async fn a_timeout_fires_on_the_hosts_own_clock() {
    let (exec, _) = host_exec();
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(TimerMachine::with_timeout(Duration::from_millis(
            50,
        )))),
        Context::new(),
        PermissionPolicy::builder().build(),
        exec,
        16,
    );
    assert!(rt.post(Event::user_message("start")).await);
    rt.wait_done().await;
    let report = rt.join().await.expect("the run ends");
    assert_eq!(report.state_label, "Fired");
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    #[tokio::test]
    async fn a_model_reply_re_enters_the_queue_and_moves_the_machine() {
        super::a_model_reply_re_enters_the_queue_and_moves_the_machine().await;
    }

    #[tokio::test]
    async fn a_timeout_fires_on_the_hosts_own_clock() {
        super::a_timeout_fires_on_the_hosts_own_clock().await;
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    async fn a_model_reply_re_enters_the_queue_and_moves_the_machine() {
        super::a_model_reply_re_enters_the_queue_and_moves_the_machine().await;
    }

    #[wasm_bindgen_test]
    async fn a_timeout_fires_on_the_hosts_own_clock() {
        super::a_timeout_fires_on_the_hosts_own_clock().await;
    }
}
