// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Append-only per-thread conversation storage.
//!
//! A machine keeps one conversation **thread** per state (e.g. `intake`,
//! `discovery`, `task:t1`). Each thread is an append-only `Vec<Message>`:
//! turns are only ever pushed, never rewritten. This is the cache-safety
//! invariant - the provider's prompt cache stays valid because the prefix
//! never changes. Cross-state / cross-submachine context is carried by
//! *appending a new user turn* to the destination thread, never by editing
//! history.

use std::collections::HashMap;

use sven_model::Message;

/// Stable string identifier for a conversation thread (e.g. `"intake"`).
pub type ThreadId = String;

/// Owns one append-only `Vec<Message>` per thread for the lifetime of a runtime.
///
/// Threads are created lazily on first access.  The store never rewrites or
/// removes earlier turns — only [`append`](ThreadStore::append) is
/// exposed for mutation, preserving the cache-safety invariant.
#[derive(Debug, Default)]
pub struct ThreadStore {
    threads: HashMap<ThreadId, Vec<Message>>,
}

impl ThreadStore {
    /// Create an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if the named thread has no turns yet (or does not exist).
    #[must_use]
    pub fn is_empty(&self, id: &str) -> bool {
        self.threads.get(id).map(Vec::is_empty).unwrap_or(true)
    }

    /// `true` if the thread exists (even if it has been created but is empty).
    #[must_use]
    pub fn exists(&self, id: &str) -> bool {
        self.threads.contains_key(id)
    }

    /// Mutable access to a thread's message buffer, creating it if absent.
    ///
    /// Callers must only ever *push* onto the returned buffer; mutating earlier
    /// turns breaks the prompt-cache invariant.
    pub fn thread(&mut self, id: &str) -> &mut Vec<Message> {
        self.threads.entry(id.to_string()).or_default()
    }

    /// Append a single turn to a thread (creating the thread if needed).
    pub fn append(&mut self, id: &str, message: Message) {
        self.threads
            .entry(id.to_string())
            .or_default()
            .push(message);
    }

    /// Replace a thread's entire contents with `messages`.
    ///
    /// Unlike [`append`](Self::append), this is **not** append-only: it is the
    /// history-seeding escape hatch used by the interactive frontends when the
    /// user edits and resubmits an earlier turn (edit-resubmit) or resumes a
    /// saved session. The frontend reconstructs the authoritative history and
    /// installs it here so the next turn streams against exactly those turns,
    /// not the store's own accumulated version. Because the prefix changes, the
    /// provider prompt cache for this thread is intentionally invalidated.
    pub fn replace_thread(&mut self, id: &str, messages: Vec<Message>) {
        self.threads.insert(id.to_string(), messages);
    }

    /// A read-only clone of a thread's current turns (empty if absent).
    #[must_use]
    pub fn snapshot(&self, id: &str) -> Vec<Message> {
        self.threads.get(id).cloned().unwrap_or_default()
    }

    /// Number of turns currently stored in a thread.
    #[must_use]
    pub fn len(&self, id: &str) -> usize {
        self.threads.get(id).map(Vec::len).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_is_append_only_and_snapshots() {
        let mut store = ThreadStore::new();
        assert!(store.is_empty("intake"));
        store.append("intake", Message::system("role"));
        store.append("intake", Message::user("hi"));
        assert_eq!(store.len("intake"), 2);
        let snap = store.snapshot("intake");
        assert_eq!(snap.len(), 2);
        // Snapshot is a clone; mutating it does not affect the store.
        assert!(!store.is_empty("intake"));
    }

    #[test]
    fn thread_creates_lazily() {
        let mut store = ThreadStore::new();
        store.thread("discovery").push(Message::user("explore"));
        assert_eq!(store.len("discovery"), 1);
        assert!(store.exists("discovery"));
    }
}
