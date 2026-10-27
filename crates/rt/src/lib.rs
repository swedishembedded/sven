// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running the same agent code
// on servers, laptops and in the browser for its clients. If your team needs
// expertise in portable Rust runtimes then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The few services agent code needs from whatever runs it: start a task,
//! wait, time out, read a monotonic clock. Everything above it is the same
//! code on every host.
//!
//! * Native: tokio (`spawn`, `time::sleep`, `time::timeout`) and
//!   `std::time::Instant`.
//! * Browser (`wasm32`): the page's or worker's own event loop through
//!   `wasm-bindgen-futures`, its timers, and `web-time`'s `Instant` (a
//!   browser has no `std::time::Instant`; calling it panics).
//!
//! A browser is single-threaded and its futures hold JavaScript values, so
//! nothing there is `Send` or `Sync`. [`MaybeSend`] and [`MaybeSync`] are
//! `Send` and `Sync` natively (as supertraits, so the compiler still knows a
//! bounded type is `Send`) and satisfied by everything in a browser: a trait
//! bounded by them keeps its native guarantees without a second copy of it
//! for the browser.

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

/// A monotonic instant that works on every host.
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;
/// A monotonic instant that works on every host.
#[cfg(target_arch = "wasm32")]
pub use web_time::Instant;

/// Wall-clock time that works on every host.
#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{SystemTime, UNIX_EPOCH};
/// Wall-clock time that works on every host.
#[cfg(target_arch = "wasm32")]
pub use web_time::{SystemTime, UNIX_EPOCH};

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

/// A future that did not finish in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed;

impl std::fmt::Display for Elapsed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// `future`'s output, or [`Elapsed`] when `duration` passes first. Natively
/// this is tokio's timeout, so a test that pauses tokio's clock pauses it.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    #[cfg(not(target_arch = "wasm32"))]
    return tokio::time::timeout(duration, future)
        .await
        .map_err(|_| Elapsed);
    #[cfg(target_arch = "wasm32")]
    {
        use futures::future::{select, Either};
        let future = std::pin::pin!(future);
        let deadline = std::pin::pin!(sleep(duration));
        match select(future, deadline).await {
            Either::Left((output, _)) => Ok(output),
            Either::Right(_) => Err(Elapsed),
        }
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

    /// A handle that aborts the task, kept while the `JoinHandle` itself is
    /// awaited (in a timeout, for instance).
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            #[cfg(not(target_arch = "wasm32"))]
            inner: self.inner.abort_handle(),
            #[cfg(target_arch = "wasm32")]
            inner: self.abort.clone(),
        }
    }
}

/// Aborts one task; see [`JoinHandle::abort_handle`].
#[derive(Debug, Clone)]
pub struct AbortHandle {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tokio::task::AbortHandle,
    #[cfg(target_arch = "wasm32")]
    inner: futures::future::AbortHandle,
}

impl AbortHandle {
    /// Stops the task at its next await.
    pub fn abort(&self) {
        self.inner.abort();
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
        forever.abort_handle().abort();
        assert!(forever.await.is_err());
        sleep_until(Instant::now()).await;
        assert_eq!(timeout(Duration::from_secs(5), async { 1 }).await, Ok(1));
        assert_eq!(
            timeout(Duration::from_millis(1), sleep(Duration::from_secs(5))).await,
            Err(Elapsed)
        );
    }
}
