// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`SessionSupervisor`] — a registry of concurrent kernel sessions.
//!
//! Each Sven surface (TUI sidebar tabs, node task executors, ACP sessions) may
//! run several independent kernel sessions at once. The supervisor is the
//! Active-Object owner of that fleet: it spawns sessions via
//! [`RuntimeBuilder::build_session`], keeps each [`SessionBundle`] alive in a
//! registry keyed by [`SessionId`], and routes events / observation
//! subscriptions by id.
//!
//! The supervisor is deliberately simple and synchronous around its registry;
//! all genuinely concurrent work happens inside each session's own runtime
//! task. Holding a `&mut SessionSupervisor` is enough to spawn or terminate
//! sessions, while `&SessionSupervisor` suffices to look one up and post to it.

use std::collections::HashMap;
use std::sync::Arc;

use sven_config::Config;
use uuid::Uuid;

use crate::context::RuntimeContext;
use crate::runtime_builder::{RuntimeBuilder, SessionBundle};

// ── SessionId ───────────────────────────────────────────────────────────────

/// Opaque identifier for a managed session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SessionId(Uuid);

impl SessionId {
    /// Mint a fresh, globally-unique session id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying UUID (for display / serialisation).
    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── SessionSupervisor ─────────────────────────────────────────────────────────

/// Owns and routes a fleet of concurrent kernel sessions.
pub struct SessionSupervisor {
    config: Arc<Config>,
    sessions: HashMap<SessionId, SessionBundle>,
}

impl SessionSupervisor {
    /// Create an empty supervisor bound to `config`.
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            sessions: HashMap::new(),
        }
    }

    /// Spawn a new kernel session in `mode` with the given runtime context,
    /// register it, and return its [`SessionId`].
    ///
    /// # Errors
    ///
    /// Propagates any error from [`RuntimeBuilder::build_session`] (unknown
    /// mode, model provider init failure, …).
    pub async fn spawn_session(
        &mut self,
        mode: impl Into<String>,
        ctx: RuntimeContext,
    ) -> anyhow::Result<SessionId> {
        let bundle = RuntimeBuilder::new(self.config.clone(), mode)
            .with_runtime_context(ctx)
            .build_session()
            .await?;
        let id = SessionId::new();
        self.sessions.insert(id, bundle);
        Ok(id)
    }

    /// Borrow a live session by id.
    #[must_use]
    pub fn get(&self, id: SessionId) -> Option<&SessionBundle> {
        self.sessions.get(&id)
    }

    /// Mutably borrow a live session by id (e.g. to drain its channels).
    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut SessionBundle> {
        self.sessions.get_mut(&id)
    }

    /// Terminate and remove a session, returning its bundle so the caller can
    /// drain any final state before it is dropped. Dropping the returned
    /// bundle shuts the kernel down.
    pub fn terminate(&mut self, id: SessionId) -> Option<SessionBundle> {
        self.sessions.remove(&id)
    }

    /// The ids of all currently-registered sessions.
    #[must_use]
    pub fn ids(&self) -> Vec<SessionId> {
        self.sessions.keys().copied().collect()
    }

    /// The number of live sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Whether the supervisor manages no sessions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_unique() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
        assert_eq!(a, a);
    }

    #[test]
    fn empty_supervisor_has_no_sessions() {
        let cfg = Arc::new(Config::default());
        let sup = SessionSupervisor::new(cfg);
        assert!(sup.is_empty());
        assert_eq!(sup.len(), 0);
        assert!(sup.ids().is_empty());
    }
}
