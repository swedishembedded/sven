// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Calls a tool runs on someone's behalf that can be stopped one by one.
//!
//! A tool call can be stopped on its own, by its id, while it runs. A tool
//! that runs calls of its own (a sub-agent's) makes each of them stoppable
//! the same way: it registers the call's id for as long as the call runs, and
//! is told when the person stops it, so it can pass the stop on to whoever
//! runs that call. The executor that runs the tool provides where they are
//! registered; a tool run without one has nothing to register with.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Where a tool registers the calls it runs on someone's behalf.
pub trait NestedCalls: Send + Sync {
    /// Registers the call `id` for as long as the returned handle lives.
    fn register(&self, id: &str) -> NestedCall;
}

/// A registered call: [`stopped`](Self::stopped) resolves when the person
/// stops it (or the turn it runs in). Dropping it unregisters the call.
pub struct NestedCall {
    stopped: Pin<Box<dyn Future<Output = ()> + Send>>,
    _registered: Box<dyn Send>,
}

impl NestedCall {
    /// A call registered by `registered` (dropped when the call ends) and
    /// stopped when `stopped` resolves.
    pub fn new(
        stopped: impl Future<Output = ()> + Send + 'static,
        registered: impl Send + 'static,
    ) -> Self {
        Self {
            stopped: Box::pin(stopped),
            _registered: Box::new(registered),
        }
    }

    /// Resolves once the call is stopped.
    pub async fn stopped(&mut self) {
        self.stopped.as_mut().await;
    }
}

tokio::task_local! {
    static CALLS: Arc<dyn NestedCalls>;
}

/// Runs `future`, a tool's call, with `calls` as where it registers the
/// calls it runs. Like [`crate::call_records::scope`], it follows the task:
/// a tool that hands its work to a task or thread of its own takes
/// [`current`] with it.
pub async fn scope<F: Future>(calls: Arc<dyn NestedCalls>, future: F) -> F::Output {
    CALLS.scope(calls, future).await
}

/// Where the running tool call registers its calls; `None` outside an
/// executor that provides it.
#[must_use]
pub fn current() -> Option<Arc<dyn NestedCalls>> {
    CALLS.try_with(Arc::clone).ok()
}
