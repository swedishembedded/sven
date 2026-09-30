// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Linked cancellation for runs and the runs they start.

use std::time::Instant;

use tokio_util::sync::CancellationToken;

/// A handle that stops a run, linked to the run that started it.
///
/// Cancelling a scope cancels every scope derived from it with
/// [`child`](Self::child), so cancelling a parent run reaches every child run
/// it started, however deep; cancelling a child leaves its parent running.
/// Clones share one state: cancelling any clone cancels them all.
#[derive(Clone, Debug, Default)]
pub struct CancelScope {
    token: CancellationToken,
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

    /// Cancels this scope at `deadline` unless it is cancelled first.
    ///
    /// The timer runs on its own task for as long as the returned guard
    /// lives, and ends early if the scope is cancelled. Whoever runs the
    /// bounded work holds the guard for exactly that long, so a run that
    /// finishes early takes its timer with it.
    #[must_use = "the deadline is dropped with the guard"]
    pub fn cancel_at(&self, deadline: Instant) -> DeadlineTimer {
        let token = self.token.clone();
        let timer = tokio::spawn(async move {
            tokio::select! {
                () = token.cancelled() => {}
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    token.cancel();
                }
            }
        });
        DeadlineTimer(timer)
    }
}

/// Keeps a [`CancelScope::cancel_at`] deadline armed; dropping it disarms it.
#[derive(Debug)]
pub struct DeadlineTimer(tokio::task::JoinHandle<()>);

impl Drop for DeadlineTimer {
    fn drop(&mut self) {
        self.0.abort();
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
