// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The few services the kernel needs from whatever runs it: start a task,
//! wait, read a monotonic clock. Everything else in the kernel is the same
//! code on every host.
//!
//! * Native: tokio (`spawn`, `time::sleep`) and `std::time::Instant`.
//! * Browser (`wasm32`): the page's or worker's own event loop through
//!   `wasm-bindgen-futures`, its timers, and `web-time`'s `Instant` (a
//!   browser has no `std::time::Instant`; calling it panics).
//!
//! A browser is single-threaded and its futures hold JavaScript values, so
//! nothing there is `Send`. [`MaybeSend`] and [`MaybeSync`] are `Send` and
//! `Sync` natively and nothing in a browser: a trait bounded by them keeps
//! its native guarantees without a second copy for the browser.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// `Send` natively; satisfied by everything in a browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}
/// `Send` natively; satisfied by everything in a browser.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// `Sync` natively; satisfied by everything in a browser.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSync: Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Sync + ?Sized> MaybeSync for T {}
/// `Sync` natively; satisfied by everything in a browser.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSync {}
#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSync for T {}

pub use sven_hsm::Instant;

/// Waits `duration`.
pub async fn sleep(duration: Duration) {
    #[cfg(not(target_arch = "wasm32"))]
    tokio::time::sleep(duration).await;
    #[cfg(target_arch = "wasm32")]
    gloo_timers::future::sleep(duration).await;
}

/// Waits until `deadline`; returns at once when it has passed.
pub async fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        sleep(deadline - now).await;
    }
}

/// Why a task's result never came: it was aborted, or it panicked.
#[cfg(not(target_arch = "wasm32"))]
pub use tokio::task::JoinError;

/// Why a task's result never came: it was aborted.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct JoinError;

#[cfg(target_arch = "wasm32")]
impl std::fmt::Display for JoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the task was aborted before it finished")
    }
}

#[cfg(target_arch = "wasm32")]
impl std::error::Error for JoinError {}

/// A started task: await it for its result, or abort it.
pub struct JoinHandle<T> {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tokio::task::JoinHandle<T>,
    #[cfg(target_arch = "wasm32")]
    result: futures::channel::oneshot::Receiver<T>,
    #[cfg(target_arch = "wasm32")]
    abort: futures::future::AbortHandle,
}

impl<T> std::fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("JoinHandle")
    }
}

impl<T> JoinHandle<T> {
    /// Stops the task at its next await. Its result never arrives.
    pub fn abort(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.inner.abort();
        #[cfg(target_arch = "wasm32")]
        self.abort.abort();
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[cfg(not(target_arch = "wasm32"))]
        return Pin::new(&mut self.inner).poll(cx);
        #[cfg(target_arch = "wasm32")]
        return Pin::new(&mut self.result)
            .poll(cx)
            .map(|result| result.map_err(|_| JoinError));
    }
}

/// Starts `task` on the host's executor.
#[cfg(not(target_arch = "wasm32"))]
pub fn spawn<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    JoinHandle {
        inner: tokio::spawn(task),
    }
}

/// Starts `task` on the browser's event loop.
#[cfg(target_arch = "wasm32")]
pub fn spawn<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let (send, result) = futures::channel::oneshot::channel();
    let (task, abort) = futures::future::abortable(task);
    wasm_bindgen_futures::spawn_local(async move {
        if let Ok(output) = task.await {
            let _ = send.send(output);
        }
    });
    JoinHandle { result, abort }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_task_answers_and_an_aborted_one_never_does() {
        assert_eq!(spawn(async { 7 }).await.unwrap(), 7);
        let forever = spawn(async {
            sleep(Duration::from_secs(3600)).await;
        });
        forever.abort();
        assert!(forever.await.is_err());
        sleep_until(Instant::now()).await;
    }
}
