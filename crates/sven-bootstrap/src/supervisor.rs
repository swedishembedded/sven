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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use sven_config::Config;
use sven_hsm::Principal;
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
    /// Owning principal per session; absent for unowned (local) sessions.
    owners: HashMap<SessionId, Principal>,
    /// Tenant → sessions index, kept in lockstep with `owners`.
    tenant_sessions: HashMap<String, HashSet<SessionId>>,
}

impl SessionSupervisor {
    /// Create an empty supervisor bound to `config`.
    #[must_use]
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            sessions: HashMap::new(),
            owners: HashMap::new(),
            tenant_sessions: HashMap::new(),
        }
    }

    /// Spawn a new kernel session in `mode` with the given runtime context,
    /// register it, and return its [`SessionId`].
    ///
    /// The session is unowned (no [`Principal`]); use
    /// [`spawn_session_for`](Self::spawn_session_for) to attribute it to a
    /// tenant/actor.
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
        self.spawn(mode, ctx, None).await
    }

    /// Spawn a new kernel session owned by `principal`, register it under the
    /// principal's tenant, and return its [`SessionId`].
    ///
    /// The principal is seeded into the kernel context so every dispatch
    /// audit record carries the tenant and actor ids.
    ///
    /// # Errors
    ///
    /// Propagates any error from [`RuntimeBuilder::build_session`] (unknown
    /// mode, model provider init failure, …).
    pub async fn spawn_session_for(
        &mut self,
        principal: Principal,
        mode: impl Into<String>,
        ctx: RuntimeContext,
    ) -> anyhow::Result<SessionId> {
        self.spawn(mode, ctx, Some(principal)).await
    }

    async fn spawn(
        &mut self,
        mode: impl Into<String>,
        ctx: RuntimeContext,
        principal: Option<Principal>,
    ) -> anyhow::Result<SessionId> {
        let mut builder =
            RuntimeBuilder::new(self.config.clone(), mode).with_runtime_context(ctx);
        if let Some(p) = principal.clone() {
            builder = builder.with_principal(p);
        }
        let bundle = builder.build_session().await?;
        let id = SessionId::new();
        self.sessions.insert(id, bundle);
        if let Some(p) = principal {
            self.tenant_sessions
                .entry(p.tenant_id.clone())
                .or_default()
                .insert(id);
            self.owners.insert(id, p);
        }
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
    /// bundle shuts the kernel down. Also unregisters the session from its
    /// owning tenant, if any.
    pub fn terminate(&mut self, id: SessionId) -> Option<SessionBundle> {
        if let Some(principal) = self.owners.remove(&id) {
            if let Some(ids) = self.tenant_sessions.get_mut(&principal.tenant_id) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.tenant_sessions.remove(&principal.tenant_id);
                }
            }
        }
        self.sessions.remove(&id)
    }

    /// The [`Principal`] that owns `id`, or `None` for unowned/unknown ids.
    #[must_use]
    pub fn principal(&self, id: SessionId) -> Option<&Principal> {
        self.owners.get(&id)
    }

    /// The ids of all live sessions owned by `tenant_id` (unordered).
    #[must_use]
    pub fn sessions_for_tenant(&self, tenant_id: &str) -> Vec<SessionId> {
        self.tenant_sessions
            .get(tenant_id)
            .map(|ids| ids.iter().copied().collect())
            .unwrap_or_default()
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
        assert!(sup.sessions_for_tenant("acme").is_empty());
    }

    fn mock_config() -> Arc<Config> {
        let mut config = Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();
        Arc::new(config)
    }

    #[tokio::test]
    async fn spawn_session_for_registers_tenant_ownership() {
        let mut sup = SessionSupervisor::new(mock_config());
        let principal = Principal::new("acme", "alice");

        let id = sup
            .spawn_session_for(principal.clone(), "chat", RuntimeContext::empty())
            .await
            .expect("session should spawn with mock provider");

        assert_eq!(sup.principal(id), Some(&principal));
        assert_eq!(sup.sessions_for_tenant("acme"), vec![id]);
        assert!(sup.sessions_for_tenant("globex").is_empty());

        // Terminating must also unregister the tenant ownership.
        let bundle = sup.terminate(id).expect("session was registered");
        drop(bundle);
        assert!(sup.principal(id).is_none());
        assert!(sup.sessions_for_tenant("acme").is_empty());
        assert!(sup.is_empty());
    }

    #[tokio::test]
    async fn spawn_session_without_principal_stays_unowned() {
        let mut sup = SessionSupervisor::new(mock_config());

        let id = sup
            .spawn_session("chat", RuntimeContext::empty())
            .await
            .expect("session should spawn with mock provider");

        assert!(sup.principal(id).is_none());
        assert!(sup.sessions_for_tenant("").is_empty());
        assert_eq!(sup.len(), 1);
        drop(sup.terminate(id));
    }
}
