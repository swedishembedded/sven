// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Keeps the OCR endpoints busy without drowning them.
//!
//! Every request takes a slot first. An endpoint's slot count (its window)
//! starts at the configured maximum, because idle capacity is waste, and
//! follows an AIMD rule from there: it is halved when the server pushes back
//! (HTTP 429/503, a full queue, a timeout) and grows by one slot per window of
//! successful requests. With several endpoints serving the same model, a slot
//! is taken on the one with the most headroom, so work spreads in proportion
//! to what each can take.
//!
//! The limiter also measures the thing it exists to prevent: the time, after
//! the first request, during which no request was in flight although pages
//! remained.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// Smallest window; one request in flight is always allowed.
const MIN_WINDOW: f64 = 1.0;
/// Pushback within this long of the previous decrease is the same event.
const DECREASE_COOLDOWN: Duration = Duration::from_secs(5);

struct Endpoint {
    window: f64,
    max: f64,
    in_flight: usize,
    served: u64,
    last_decrease: Option<Instant>,
}

struct State {
    endpoints: Vec<Endpoint>,
    peak_in_flight: usize,
    idle: Duration,
    idle_since: Option<Instant>,
}

/// What the limiter measured, for the report.
#[derive(Debug, Clone, Default)]
pub struct LimiterStats {
    pub peak_in_flight: usize,
    /// Time with nothing in flight while pages remained.
    pub idle_ms: u64,
    pub requests_per_endpoint: Vec<u64>,
}

pub struct Limiter {
    state: Mutex<State>,
    freed: Notify,
    /// Pages not yet settled; idle time only counts while this is positive.
    remaining: AtomicUsize,
}

/// One request's slot; returned on drop.
pub struct Slot {
    limiter: Arc<Limiter>,
    endpoint: usize,
}

impl Slot {
    /// Which endpoint (index into the providers) to send the request to.
    pub fn endpoint(&self) -> usize {
        self.endpoint
    }

    /// The request was answered: let the window grow.
    pub fn succeeded(&self) {
        let mut s = self.limiter.state.lock().unwrap();
        let ep = &mut s.endpoints[self.endpoint];
        ep.window = (ep.window + 1.0 / ep.window).min(ep.max);
        ep.served += 1;
    }

    /// The server pushed back: halve the window.
    pub fn overloaded(&self) {
        let mut s = self.limiter.state.lock().unwrap();
        let ep = &mut s.endpoints[self.endpoint];
        let fresh = ep
            .last_decrease
            .is_none_or(|t| t.elapsed() >= DECREASE_COOLDOWN);
        if fresh {
            ep.window = (ep.window / 2.0).max(MIN_WINDOW);
            ep.last_decrease = Some(Instant::now());
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        {
            let mut s = self.limiter.state.lock().unwrap();
            s.endpoints[self.endpoint].in_flight -= 1;
            let total: usize = s.endpoints.iter().map(|e| e.in_flight).sum();
            if total == 0 && self.limiter.remaining.load(Ordering::SeqCst) > 0 {
                s.idle_since = Some(Instant::now());
            }
        }
        self.limiter.freed.notify_waiters();
    }
}

impl Limiter {
    /// `max` slots at each of `endpoints` endpoints, `pages` pages to do.
    pub fn new(endpoints: usize, max: usize, pages: usize) -> Arc<Self> {
        let max = max.max(1) as f64;
        Arc::new(Self {
            state: Mutex::new(State {
                endpoints: (0..endpoints.max(1))
                    .map(|_| Endpoint {
                        window: max,
                        max,
                        in_flight: 0,
                        served: 0,
                        last_decrease: None,
                    })
                    .collect(),
                peak_in_flight: 0,
                idle: Duration::ZERO,
                idle_since: None,
            }),
            freed: Notify::new(),
            remaining: AtomicUsize::new(pages),
        })
    }

    /// A page is settled (accepted, warned or failed).
    pub fn page_done(&self) {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            // The wait after the last request is the end of the run, not idleness.
            self.state.lock().unwrap().idle_since = None;
        }
    }

    /// Waits for headroom on some endpoint and takes a slot there.
    pub async fn acquire(self: &Arc<Self>) -> Slot {
        loop {
            let notified = self.freed.notified();
            tokio::pin!(notified);
            // Register interest before checking, so a release in between is not lost.
            notified.as_mut().enable();
            if let Some(slot) = self.try_acquire() {
                return slot;
            }
            notified.await;
        }
    }

    fn try_acquire(self: &Arc<Self>) -> Option<Slot> {
        let mut s = self.state.lock().unwrap();
        let pick = s
            .endpoints
            .iter()
            .enumerate()
            .filter(|(_, e)| (e.in_flight as f64) < e.window.floor().max(MIN_WINDOW))
            .min_by(|(_, a), (_, b)| {
                let load = |e: &Endpoint| e.in_flight as f64 / e.window;
                load(a).total_cmp(&load(b))
            })
            .map(|(i, _)| i)?;
        s.endpoints[pick].in_flight += 1;
        let total: usize = s.endpoints.iter().map(|e| e.in_flight).sum();
        s.peak_in_flight = s.peak_in_flight.max(total);
        if let Some(since) = s.idle_since.take() {
            s.idle += since.elapsed();
        }
        Some(Slot {
            limiter: Arc::clone(self),
            endpoint: pick,
        })
    }

    pub fn stats(&self) -> LimiterStats {
        let s = self.state.lock().unwrap();
        LimiterStats {
            peak_in_flight: s.peak_in_flight,
            idle_ms: s.idle.as_millis() as u64,
            requests_per_endpoint: s.endpoints.iter().map(|e| e.served).collect(),
        }
    }
}

/// Whether an error from the model layer is the server pushing back.
pub fn is_pushback(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "429",
        "503",
        "too many requests",
        "overloaded",
        "queue",
        "rate limit",
        "timed out",
        "busy",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slots_are_bounded_spread_and_the_window_follows_pushback() {
        let limiter = Limiter::new(2, 2, 10);
        let a = limiter.acquire().await;
        let b = limiter.acquire().await;
        assert_ne!(a.endpoint(), b.endpoint(), "work spreads across endpoints");
        let c = limiter.acquire().await;
        let d = limiter.acquire().await;
        // All four slots (2 endpoints x 2) are taken: the fifth must wait.
        let waiting = tokio::time::timeout(Duration::from_millis(50), limiter.acquire()).await;
        assert!(waiting.is_err());
        c.overloaded();
        drop((a, b, c, d));
        // Endpoint of `c` now has a window of 1.
        let s1 = limiter.acquire().await;
        let s2 = limiter.acquire().await;
        let s3 = limiter.acquire().await;
        let per: Vec<usize> = [&s1, &s2, &s3].iter().map(|s| s.endpoint()).collect();
        assert!(per.iter().filter(|e| **e == per[0]).count() <= 2, "{per:?}");
        assert_eq!(limiter.stats().peak_in_flight, 4);
    }

    #[test]
    fn pushback_is_recognised_from_the_error_text() {
        assert!(is_pushback("brain error 429 Too Many Requests"));
        assert!(is_pushback("model request timed out"));
        assert!(!is_pushback("brain error 400 Bad Request"));
    }
}
