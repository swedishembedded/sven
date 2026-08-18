// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A task handle that aborts on drop.

use tokio::task::JoinHandle;

/// A [`JoinHandle`] that aborts its task when dropped.
///
/// The kernel consumer loop is handed an owning clone of the event `EventSink`,
/// so its `rx.recv()` never returns `None` and the loop cannot end on its own
/// unless the machine reports done — which `ReactiveAgentMachine` never does.
/// Without this, dropping a `Runtime`/`ErasedRuntime` left a parked task
/// pinning the whole object graph behind it: the `Context` (both audit
/// vectors), the boxed machine, the executor's conversation store, the tool
/// registry including live MCP handles, and the provider's HTTP pool. Every
/// TUI model switch, session delete, and ACP teardown leaked one.
///
/// [`Self::disarm`] hands the handle back for an orderly `join`, so awaiting a
/// report is unaffected.
pub(crate) struct AbortOnDrop<T>(Option<JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    pub(crate) fn new(handle: JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    pub(crate) fn abort(&self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }

    /// Takes the handle, so dropping this wrapper no longer aborts the task.
    pub(crate) fn disarm(mut self) -> JoinHandle<T> {
        self.0.take().expect("handle is taken exactly once, by join")
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.abort();
        }
    }
}
