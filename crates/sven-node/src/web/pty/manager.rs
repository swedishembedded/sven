// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! `PtyManager` — maps device UUIDs to their PTY sessions and manages the
//! lifecycle of those sessions.
//!
//! # Session lifecycle
//!
//! ```text
//! Device approved → PtyManager::get_or_spawn()
//!     → session exists? attach (tmux -A reattaches)
//!     → no session? spawn new (LocalSpawner::spawn)
//!
//! Browser disconnects → WebSocket handler drops its PtySession handle
//!     → tmux survives (manages its own process group)
//!
//! Device revoked → PtyManager::kill(device_id)
//!     → kill PTY child (kills tmux)
//!     → remove from map
//! ```
//!
//! PTY session records are kept even after browser disconnect because tmux
//! persists.  The browser reattaches on next login.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use tokio::sync::Mutex;
use uuid::Uuid;

use super::{
    spawner::{SessionConfig, SessionSpawner},
    PtySession,
};

/// Thread-safe PTY session manager.
///
/// Clone is cheap — the inner map is `Arc<Mutex<...>>`.
#[derive(Clone)]
pub struct PtyManager {
    sessions: Arc<Mutex<HashMap<Uuid, SessionSlot>>>,
    spawner: Arc<dyn SessionSpawner>,
    default_cols: u16,
    default_rows: u16,
    default_pty_command: Vec<String>,
    default_working_dir: PathBuf,
    /// Environment variables merged into every spawned PTY session.
    ///
    /// Used to inject `SVEN_GATEWAY_TOKEN`, `SVEN_GATEWAY_URL`, and
    /// `SVEN_GATEWAY_INSECURE` so the in-terminal sven process can
    /// authenticate to this node automatically.
    session_env: HashMap<String, String>,
}

/// Bookkeeping for a running PTY session.
struct SessionSlot {
    /// True while a WebSocket connection is actively attached.
    attached: bool,
}

impl PtyManager {
    pub fn new(
        spawner: Arc<dyn SessionSpawner>,
        pty_command: Vec<String>,
        working_dir: PathBuf,
        session_env: HashMap<String, String>,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            spawner,
            default_cols: 220,
            default_rows: 50,
            default_pty_command: pty_command,
            default_working_dir: working_dir,
            session_env,
        }
    }

    /// Spawn a new PTY session for `device_id`.
    ///
    /// The `short_id` (first 8 hex chars of the UUID) is substituted for
    /// `{id}` in the command template, so tmux sessions are named
    /// `sven-<short_id>`.
    pub async fn spawn(&self, device_id: Uuid, cols: u16, rows: u16) -> anyhow::Result<PtySession> {
        let short_id = &device_id.to_string().replace('-', "")[..8];
        let command: Vec<String> = self
            .default_pty_command
            .iter()
            .map(|s| s.replace("{id}", short_id))
            .collect();

        let config = SessionConfig {
            cols,
            rows,
            working_dir: self.default_working_dir.clone(),
            env: self.session_env.clone(),
            command,
        };

        let mut session = self.spawner.spawn(config).await?;
        session.id = device_id;

        let mut map = self.sessions.lock().await;
        map.insert(device_id, SessionSlot { attached: false });

        Ok(session)
    }

    /// Mark a session as attached (WebSocket connected).
    pub async fn mark_attached(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        if let Some(slot) = map.get_mut(&device_id) {
            slot.attached = true;
        }
    }

    /// Mark a session as detached (WebSocket disconnected).
    pub async fn mark_detached(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        if let Some(slot) = map.get_mut(&device_id) {
            slot.attached = false;
        }
    }

    /// Remove a session record (called after kill or natural exit).
    pub async fn remove(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        map.remove(&device_id);
    }

    /// True if any session record exists for this device.
    pub async fn has_session(&self, device_id: Uuid) -> bool {
        let map = self.sessions.lock().await;
        map.contains_key(&device_id)
    }

    /// Number of tracked sessions.
    pub async fn session_count(&self) -> usize {
        let map = self.sessions.lock().await;
        map.len()
    }

    /// Default columns for new sessions.
    pub fn default_cols(&self) -> u16 {
        self.default_cols
    }

    /// Default rows for new sessions.
    pub fn default_rows(&self) -> u16 {
        self.default_rows
    }
}
