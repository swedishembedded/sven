// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! Generic thread-safe shared list for live-refreshable discovery collections.
//!
//! [`Shared<T>`] is a thin wrapper around `Arc<RwLock<Arc<[T]>>>` that
//! provides cheap snapshot reads and atomic batch replacements.  It is used by
//! [`SharedSkills`][crate::SharedSkills] and [`SharedAgents`][crate::SharedAgents]
//! to share a single discovery result between the TUI thread and the background
//! agent task, so that a `/refresh` command updates both consumers without
//! restarting the agent.

use std::sync::{Arc, RwLock};

/// A thread-safe, live-refreshable ordered list.
///
/// ## Usage pattern
///
/// 1. Create once at startup with the initial discovery result.
/// 2. Pass clones to every consumer (TUI command registry, agent task, etc.).
/// 3. Call [`set`][Self::set] to atomically replace the contents.  All clones
///    immediately see the new data on their next [`get`][Self::get] call.
pub struct Shared<T: Send + Sync + 'static>(Arc<RwLock<Arc<[T]>>>);

impl<T: Send + Sync + 'static> Shared<T> {
    /// Create from an initial list.
    pub fn new(items: Vec<T>) -> Self {
        Self(Arc::new(RwLock::new(items.into_boxed_slice().into())))
    }

    /// Create an empty instance (zero items).
    pub fn empty() -> Self {
        Self::new(vec![])
    }

    /// Return a cheap snapshot of the current contents.
    ///
    /// The returned `Arc` is valid until the next [`set`][Self::set] call and
    /// can be iterated, indexed, or passed around freely.
    #[must_use]
    pub fn get(&self) -> Arc<[T]> {
        self.0.read().expect("Shared lock poisoned").clone()
    }

    /// Atomically replace the contents with a new list.
    ///
    /// Existing snapshots obtained from prior [`get`][Self::get] calls remain
    /// valid; they simply refer to the old data.
    pub fn set(&self, items: Vec<T>) {
        let new: Arc<[T]> = items.into_boxed_slice().into();
        *self.0.write().expect("Shared lock poisoned") = new;
    }
}

impl<T: Send + Sync + 'static> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Send + Sync + 'static> std::fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.0.read().map(|g| g.len()).unwrap_or(0);
        write!(f, "Shared({len} items)")
    }
}

impl<T: Send + Sync + 'static> Default for Shared<T> {
    fn default() -> Self {
        Self::empty()
    }
}
