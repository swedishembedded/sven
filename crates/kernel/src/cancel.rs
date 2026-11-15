// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Linked cancellation for runs and the runs they start.

use crate::rt::{self, Instant};

use tokio_util::sync::CancellationToken;

/// A handle that stops a run, linked to the run that started it.
///
/// Cancelling a scope cancels every scope derived from it with
/// [`child`](Self::child), so cancelling a parent run reaches every child run
/// it started, however deep; cancelling a child leaves its parent running.
/// Clones share one state: cancelling any clone cancels them all.
///
/// A scope also keeps the [calls](Self::call) running under it by id, so one
/// of them can be cancelled on its own ([`cancel_call`](Self::cancel_call))
/// while the scope goes on.
#[derive(Clone, Debug, Default)]
pub struct CancelScope {
    token: CancellationToken,
    calls: CallTokens,
}

/// The calls running under a scope, by id.
type CallTokens =
    std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, CancellationToken>>>;

/// The scope of one call running under a [`CancelScope`]: cancelled with
/// that scope, or on its own by its id; it stops being findable by its id
/// once dropped (the call ended).
#[derive(Debug)]
pub struct CallScope {
    token: CancellationToken,
    calls: CallTokens,
    id: String,
}

impl CallScope {
    /// `true` once the call, or the scope it runs under, is cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once the call, or the scope it runs under, is cancelled.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

impl Drop for CallScope {
    fn drop(&mut self) {
        // A call's id is its own (the model's id for the call): nothing else
        // is registered under it.
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

impl CancelScope {
    /// A scope linked to nothing: only cancelling it (or a clone) stops it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A scope cancelled whenever this one is, which can also be cancelled
    /// on its own without affecting this one.
    #[must_use]
    pub fn child(&self) -> Self {
        Self {
            token: self.token.child_token(),
            calls: CallTokens::default(),
        }
    }

    /// The scope of the call `id` running under this one, findable by `id`
    /// (see [`cancel_call`](Self::cancel_call)) for as long as it lives.
    #[must_use = "the call can be cancelled only while its scope lives"]
    pub fn call(&self, id: &str) -> CallScope {
        let token = self.token.child_token();
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string(), token.clone());
        CallScope {
            token,
            calls: std::sync::Arc::clone(&self.calls),
            id: id.to_string(),
        }
    }

    /// Cancels only the call `id` running under this scope; `false` when no
    /// such call is running (it ended, or never ran here).
    pub fn cancel_call(&self, id: &str) -> bool {
        let calls = self
            .calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match calls.get(id) {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancels this scope and every scope derived from it.
    pub fn cancel(&self) {
        self.token.cancel();
    }

    /// `true` once this scope, or one it derives from, has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Resolves once this scope is cancelled.
    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    /// A guard that cancels this scope when dropped.
    #[must_use = "the scope is cancelled as soon as the guard is dropped"]
    pub(crate) fn cancel_on_drop(&self) -> CancelOnDrop {
        CancelOnDrop(self.clone())
    }

    /// Cancels this scope at `deadline` unless it is cancelled first.
    ///
    /// The timer runs on its own task for as long as the returned guard
    /// lives, and ends early if the scope is cancelled. Whoever runs the
    /// bounded work holds the guard for exactly that long, so a run that
    /// finishes early takes its timer with it.
    #[must_use = "the deadline is dropped with the guard"]
    pub fn cancel_at(&self, deadline: Instant) -> DeadlineTimer {
        let token = self.token.clone();
        let timer = rt::spawn(async move {
            tokio::select! {
                () = token.cancelled() => {}
                () = rt::sleep_until(deadline) => {
                    token.cancel();
                }
            }
        });
        DeadlineTimer(timer)
    }
}

/// Cancels its scope when dropped (see [`CancelScope::cancel_on_drop`]).
pub(crate) struct CancelOnDrop(CancelScope);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Keeps a [`CancelScope::cancel_at`] deadline armed; dropping it disarms it.
#[derive(Debug)]
pub struct DeadlineTimer(rt::JoinHandle<()>);

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The scope of the turn in flight within a run, shared between the run's
/// loop and whoever may stop the turn.
///
/// A run can outlive many turns (a session that answers message after
/// message). Stopping a turn cancels only the current turn scope - a child
/// of the run's - which ends what that turn started (a spawned tool call, an
/// effect awaited inline) while the run goes on; the loop then
/// [renews](Self::renew) the scope so the next turn starts with nothing
/// cancelled.
#[derive(Clone, Debug)]
pub struct TurnScope {
    current: std::sync::Arc<std::sync::Mutex<CancelScope>>,
}

impl TurnScope {
    /// A turn scope under `run`.
    #[must_use]
    pub fn new(run: &CancelScope) -> Self {
        Self {
            current: std::sync::Arc::new(std::sync::Mutex::new(run.child())),
        }
    }

    /// The scope of the turn in flight now.
    #[must_use]
    pub fn current(&self) -> CancelScope {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Stops the turn in flight: everything started under its scope.
    pub fn cancel(&self) {
        self.current().cancel();
    }

    /// Cancels only the call `id` of the turn in flight; the turn goes on.
    /// `false` when no such call is running.
    pub fn cancel_call(&self, id: &str) -> bool {
        self.current().cancel_call(id)
    }

    /// When the current turn was stopped, starts a fresh one under `run` and
    /// returns its scope; `None` while the current one is still live, or
    /// once the run itself is over.
    pub fn renew(&self, run: &CancelScope) -> Option<CancelScope> {
        let mut current = self
            .current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !current.is_cancelled() || run.is_cancelled() {
            return None;
        }
        *current = run.child();
        Some(current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn cancelling_a_parent_reaches_its_children_but_not_the_reverse() {
        let parent = CancelScope::new();
        let child = parent.child();
        let grandchild = child.child();
        let sibling = parent.child();

        child.cancel();
        assert!(child.is_cancelled() && grandchild.is_cancelled());
        assert!(!parent.is_cancelled() && !sibling.is_cancelled());

        parent.cancel();
        assert!(sibling.is_cancelled());
    }

    /// One call of a turn is cancelled on its own, by its id: it stops, and
    /// the turn and the other calls go on. A call that has ended is not
    /// there to cancel; stopping the turn stops every call.
    #[test]
    fn one_call_is_cancelled_by_its_id_and_the_turn_goes_on() {
        let run = CancelScope::new();
        let turn = TurnScope::new(&run);
        let slow = turn.current().call("call_slow");
        let other = turn.current().call("call_other");

        assert!(turn.cancel_call("call_slow"));
        assert!(slow.is_cancelled());
        assert!(!other.is_cancelled() && !turn.current().is_cancelled());

        let ended = turn.current().call("call_done");
        drop(ended);
        assert!(!turn.cancel_call("call_done"), "an ended call is not there");
        assert!(!turn.cancel_call("never"));

        turn.cancel();
        assert!(other.is_cancelled());
    }

    #[tokio::test]
    async fn a_deadline_cancels_the_scope_when_it_passes() {
        let scope = CancelScope::new();
        let _timer = scope.cancel_at(Instant::now() + Duration::from_millis(20));
        tokio::time::timeout(Duration::from_secs(5), scope.cancelled())
            .await
            .expect("the deadline cancels the scope");
    }

    #[tokio::test]
    async fn a_dropped_deadline_leaves_no_timer_behind() {
        let alive = || {
            tokio::runtime::Handle::current()
                .metrics()
                .num_alive_tasks()
        };
        let scope = CancelScope::new();
        let before = alive();
        let timer = scope.cancel_at(Instant::now() + Duration::from_secs(3600));
        assert_eq!(alive(), before + 1);
        drop(timer);
        for _ in 0..100 {
            if alive() == before {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(alive(), before, "the timer task ended with its guard");
        assert!(!scope.is_cancelled(), "disarming does not cancel");
    }
}
