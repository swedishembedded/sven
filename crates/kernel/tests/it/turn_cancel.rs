// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Stopping a turn stops what the turn started, and only that.
//!
//! A long-lived session (`sven rc`, an attached terminal) runs many turns in
//! one run. Cancelling a turn must end the work in flight for it - a tool
//! spawned concurrently, an effect awaited inline such as a model stream -
//! without ending the run, and the next turn must start with nothing
//! cancelled.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use sven_hsm::{
    Context, Effect, Event, Hsm, ObservationSink, PermissionPolicy, Reaction, ToolCapability,
};
use sven_kernel::{CancelScope, EffectExecutor, ErasedRuntime, EventSink};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum S {
    Top,
    Idle,
    Busy,
}

/// Idle --message--> Busy (one tool call) --result or cancel--> Idle.
struct OneTool {
    id: sven_hsm::MachineId,
}

impl sven_hsm::Machine for OneTool {
    type State = S;
    fn id(&self) -> sven_hsm::MachineId {
        self.id
    }
    fn top(&self) -> S {
        S::Top
    }
    fn initial(&self) -> S {
        S::Idle
    }
    fn superstate(&self, _state: S) -> S {
        S::Top
    }
    fn all_states(&self) -> Vec<S> {
        vec![S::Idle, S::Busy]
    }
    fn dispatch_state(&mut self, state: S, event: &Event, _ctx: &mut Context) -> Reaction<S> {
        match (state, event) {
            (S::Idle, Event::UserMessage { .. }) => Reaction::transition(
                S::Busy,
                [Effect::CallTool {
                    call_id: sven_hsm::ToolCallId::new(),
                    name: "slow".into(),
                    capability: ToolCapability::ReadFile,
                    args: Value::Null,
                }],
                "run the tool",
            ),
            (S::Busy, Event::ToolFailed { .. }) => {
                Reaction::transition(S::Idle, [], "the turn is over")
            }
            // The turn's last result ends it, and says so.
            (S::Busy, Event::ToolSucceeded { .. }) => Reaction::transition(
                S::Idle,
                [Effect::Announce {
                    event: Box::new(sven_hsm::UiEvent::TurnComplete),
                }],
                "the turn is done",
            ),
            // The stop ends the turn, and says so: what a surface waits on.
            (S::Busy, Event::UserCancelled) => Reaction::transition(
                S::Idle,
                [Effect::Announce {
                    event: Box::new(sven_hsm::UiEvent::Aborted {
                        partial_text: String::new(),
                    }),
                }],
                "the person stopped the turn",
            ),
            (S::Top, _) => Reaction::Ignored,
            _ => Reaction::parent(S::Top),
        }
    }
}

/// Counts when it is dropped: the work it guards was stopped.
struct Stopped(Arc<AtomicUsize>);
impl Drop for Stopped {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A tool that takes a minute unless stopped. `inline` awaits it inside
/// `execute` (as a model stream is); otherwise it is spawned (as a tool is)
/// and stops through the scope it was given, reporting that it failed.
struct SlowTool {
    scope: CancelScope,
    stopped: Arc<AtomicUsize>,
    /// Told each time a call starts: an inline call holds the runtime until
    /// it ends, so the state it has entered is not published before then.
    started: Arc<tokio::sync::Notify>,
    /// The turn's endings announced: the stop's own effects ran.
    ended: Arc<AtomicUsize>,
    inline: bool,
}

#[async_trait]
impl EffectExecutor for SlowTool {
    fn bind_cancel(&mut self, scope: &CancelScope) {
        self.scope = scope.clone();
    }

    async fn execute(&mut self, effect: Effect, sink: &EventSink, _obs: &ObservationSink) {
        let Effect::CallTool { call_id, .. } = effect else {
            if matches!(effect, Effect::Announce { .. }) {
                self.ended.fetch_add(1, Ordering::SeqCst);
            }
            return;
        };
        let guard = Stopped(Arc::clone(&self.stopped));
        let sink = sink.clone();
        self.started.notify_one();
        if self.inline {
            tokio::time::sleep(Duration::from_secs(60)).await;
            std::mem::forget(guard);
            sink.emit(Event::ToolSucceeded {
                call_id,
                observation: Value::Null,
            })
            .await;
            return;
        }
        let scope = self.scope.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = scope.cancelled() => {
                    drop(guard);
                    sink.emit(Event::ToolFailed { call_id, error: "stopped".into() }).await;
                }
                () = tokio::time::sleep(Duration::from_secs(60)) => {
                    std::mem::forget(guard);
                    sink.emit(Event::ToolSucceeded { call_id, observation: Value::Null }).await;
                }
            }
        });
    }
}

async fn cancelling_a_turn(inline: bool) {
    let stopped = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(AtomicUsize::new(0));
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(OneTool {
            id: sven_hsm::MachineId::new(),
        })),
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        SlowTool {
            scope: CancelScope::new(),
            stopped: Arc::clone(&stopped),
            started: Arc::clone(&started),
            ended: Arc::clone(&ended),
            inline,
        },
        16,
    );

    assert!(rt.post(Event::user_message("go")).await);
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the turn's work started");

    // What a person pressing Stop does: stop the turn's work, then tell the
    // machine.
    rt.cancel_turn();
    assert!(rt.post(Event::UserCancelled).await);
    tokio::time::timeout(Duration::from_secs(2), rt.wait_for_state("Idle"))
        .await
        .expect("the turn ended at once, not when its work would have finished");
    tokio::time::timeout(Duration::from_secs(2), async {
        while stopped.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn's work was stopped");
    assert!(!rt.status().done, "stopping a turn does not end the run");
    // The stop is dispatched while the turn it stops is cancelled: what it
    // does is the next turn's, never dropped with the stopped one's work.
    tokio::time::timeout(Duration::from_secs(2), async {
        while ended.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the stop announced that the turn ended");

    // The next turn starts with nothing cancelled: its work runs on.
    assert!(rt.post(Event::user_message("again")).await);
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the next turn's work started");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        stopped.load(Ordering::SeqCst),
        1,
        "the next turn's work was stopped too"
    );
    rt.abort();
}

#[tokio::test]
async fn cancelling_a_turn_stops_a_spawned_tool_and_not_the_next_one() {
    cancelling_a_turn(false).await;
}

#[tokio::test]
async fn cancelling_a_turn_drops_work_awaited_inline() {
    cancelling_a_turn(true).await;
}

/// The turn's last result is already queued when the person stops it: the
/// machine ends the turn on that result, whose announcement must still reach
/// the surface, since the stop that follows finds the machine at rest with
/// nothing to end. The turn ends for the surface exactly once.
#[tokio::test]
async fn a_stop_that_comes_after_the_turns_last_result_still_ends_it_once() {
    let stopped = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let ended = Arc::new(AtomicUsize::new(0));
    let rt = ErasedRuntime::spawn(
        Box::new(Hsm::new(OneTool {
            id: sven_hsm::MachineId::new(),
        })),
        Context::new(),
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile])
            .build(),
        SlowTool {
            scope: CancelScope::new(),
            stopped: Arc::clone(&stopped),
            started: Arc::clone(&started),
            ended: Arc::clone(&ended),
            inline: false,
        },
        16,
    );
    assert!(rt.post(Event::user_message("go")).await);
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the turn's work started");
    tokio::time::timeout(Duration::from_secs(2), rt.wait_for_state("Busy"))
        .await
        .expect("the turn runs");

    // The result arrives; before it is dispatched, the person stops the turn.
    rt.cancel_turn();
    assert!(
        rt.post(Event::ToolSucceeded {
            call_id: sven_hsm::ToolCallId::new(),
            observation: Value::Null,
        })
        .await
    );
    assert!(rt.post(Event::UserCancelled).await);
    tokio::time::timeout(Duration::from_secs(2), async {
        while ended.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the turn's end reached the surface");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ended.load(Ordering::SeqCst), 1, "and only once");
    rt.abort();
}
