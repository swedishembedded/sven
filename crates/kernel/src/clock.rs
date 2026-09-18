// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Time, abstracted so timeouts are deterministic in tests.
//!
//! The kernel schedules timeouts through a [`Clock`] rather than calling
//! `tokio::time` directly, which is what lets a test advance time instead of
//! waiting for it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use sven_hsm::{Event, TimerId};

use crate::EventSink;

/// A monotonic, awaitable clock. Abstracted so timeouts are deterministic in
/// tests.
///
/// The fundamental operation is [`sleep_until`](Clock::sleep_until) with an
/// *absolute* deadline. Schedulers compute the deadline once, synchronously, at
/// scheduling time; this is what makes a [`VirtualClock`] race-free even when
/// time is advanced before the sleeping task starts.
#[async_trait]
pub trait Clock: Send + Sync + 'static {
    /// Time elapsed since the clock's epoch.
    fn now(&self) -> Duration;
    /// Resolves once clock-time reaches `deadline`.
    async fn sleep_until(&self, deadline: Duration);
    /// Resolves once `duration` of clock-time has elapsed from now.
    async fn sleep(&self, duration: Duration) {
        let deadline = self.now() + duration;
        self.sleep_until(deadline).await;
    }
}

/// Real wall-clock time backed by `tokio::time`.
pub struct SystemClock {
    start: std::time::Instant,
}

impl SystemClock {
    /// Creates a clock whose epoch is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    async fn sleep_until(&self, deadline: Duration) {
        let now = self.now();
        if deadline > now {
            tokio::time::sleep(deadline - now).await;
        }
    }
}

/// A manually-driven virtual clock for deterministic timer tests.
///
/// Time starts at zero and only moves when [`advance`](VirtualClock::advance) is
/// called; sleeping tasks wake the instant the virtual time reaches their
/// deadline. Implemented with a `watch` channel so wakeups are edge-triggered
/// rather than polled.
#[derive(Clone)]
pub struct VirtualClock {
    tx: Arc<watch::Sender<Duration>>,
}

impl VirtualClock {
    /// Creates a virtual clock at time zero.
    #[must_use]
    pub fn new() -> Self {
        let (tx, _rx) = watch::channel(Duration::ZERO);
        Self { tx: Arc::new(tx) }
    }

    /// Advances virtual time by `delta`, waking any sleepers whose deadline has
    /// now passed.
    pub fn advance(&self, delta: Duration) {
        self.tx.send_modify(|t| *t += delta);
    }
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Clock for VirtualClock {
    fn now(&self) -> Duration {
        *self.tx.borrow()
    }

    async fn sleep_until(&self, deadline: Duration) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow() >= deadline {
                return;
            }
            if rx.changed().await.is_err() {
                return; // clock dropped
            }
        }
    }
}

/// Schedules one-shot timeouts that post `Event::Timeout { timer_id }` when they
/// elapse, using an injected [`Clock`]. Cancellation aborts the pending task.
pub struct TimerService {
    clock: Arc<dyn Clock>,
    sink: EventSink,
    tasks: HashMap<TimerId, JoinHandle<()>>,
}

impl TimerService {
    /// Creates a timer service that fires events into `sink` using `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, sink: EventSink) -> Self {
        Self {
            clock,
            sink,
            tasks: HashMap::new(),
        }
    }

    /// Schedules `timer_id` to fire after `duration` of clock-time. A timer with
    /// the same id is replaced.
    pub fn schedule(&mut self, timer_id: TimerId, duration: Duration) {
        let clock = Arc::clone(&self.clock);
        let sink = self.sink.clone();
        // Compute the absolute deadline now, synchronously, so advancing a
        // VirtualClock before the spawned task starts cannot be missed.
        let deadline = self.clock.now() + duration;
        let handle = tokio::spawn(async move {
            clock.sleep_until(deadline).await;
            let _ = sink.emit(Event::timeout(timer_id)).await;
        });
        if let Some(old) = self.tasks.insert(timer_id, handle) {
            old.abort();
        }
    }

    /// Cancels a scheduled timer, if present.
    pub fn cancel(&mut self, timer_id: TimerId) {
        if let Some(handle) = self.tasks.remove(&timer_id) {
            handle.abort();
        }
    }
}
