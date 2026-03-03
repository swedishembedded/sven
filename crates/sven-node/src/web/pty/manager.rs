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
//! Device approved → PtyManager::spawn()
//!     → session running?  return new reader attached to same process
//!     → no session?       spawn new process
//!
//! Browser disconnects → WebSocket handler closes
//!     → process KEEPS RUNNING (stdin/master/child held in the slot)
//!
//! Browser reconnects → PtyManager::spawn()
//!     → session still running → clone new reader, attach without restart
//!     → process exited → remove slot, spawn fresh
//!
//! Device revoked → PtyManager::kill(device_id)
//!     → kill PTY child, remove slot
//! ```

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

use super::{
    spawner::{SessionConfig, SessionSpawner},
    PtySession,
};

/// Persistent state for one device's PTY session.
///
/// Held by the manager across WebSocket disconnects so the underlying process
/// survives and the browser can reattach without a restart.
struct SessionSlot {
    /// PTY master — source for fresh readers on every reconnect.
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    /// Write end of the PTY; shared so the WS handler can write to it.
    stdin: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    /// Child process; used for exit detection and cleanup.
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    /// True while a WebSocket connection is actively attached.
    attached: bool,
}

impl SessionSlot {
    /// Returns `true` if the child process is still running.
    fn is_alive(&self) -> bool {
        let mut child = match self.child.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        // try_wait returns Ok(None) → still running, Ok(Some(_)) → exited.
        matches!(child.try_wait(), Ok(None))
    }
}

/// Thread-safe PTY session manager.
///
/// Clone is cheap — the inner map is `Arc<Mutex<...>>`.
#[derive(Clone)]
pub struct PtyManager {
    sessions: Arc<TokioMutex<HashMap<Uuid, SessionSlot>>>,
    spawner: Arc<dyn SessionSpawner>,
    default_cols: u16,
    default_rows: u16,
    default_pty_command: Vec<String>,
    default_working_dir: PathBuf,
    /// Environment variables merged into every spawned PTY session.
    ///
    /// Used to inject `SVEN_NODE_TOKEN`, `SVEN_NODE_URL`, and
    /// `SVEN_GATEWAY_INSECURE` (legacy) so the in-terminal sven process can
    /// authenticate to this node automatically.
    session_env: HashMap<String, String>,
}

impl PtyManager {
    pub fn new(
        spawner: Arc<dyn SessionSpawner>,
        pty_command: Vec<String>,
        working_dir: PathBuf,
        session_env: HashMap<String, String>,
    ) -> Self {
        Self {
            sessions: Arc::new(TokioMutex::new(HashMap::new())),
            spawner,
            default_cols: 220,
            default_rows: 50,
            default_pty_command: pty_command,
            default_working_dir: working_dir,
            session_env,
        }
    }

    /// Return a `PtySession` for `device_id`.
    ///
    /// If a session already exists **and the child is still running**, the
    /// existing process is reused: a new reader is cloned from the master PTY
    /// (so the browser gets fresh output from the current position) without
    /// restarting the process.
    ///
    /// If no session exists, or the previous process has exited, a fresh
    /// process is spawned and a new slot is created.
    pub async fn spawn(&self, device_id: Uuid, cols: u16, rows: u16) -> anyhow::Result<PtySession> {
        let mut map = self.sessions.lock().await;

        // Check if there is a live session we can reattach to.
        if let Some(slot) = map.get_mut(&device_id) {
            if slot.is_alive() {
                // Clone a fresh reader from the master so this WS connection
                // sees current output.
                let reader = {
                    let master = slot
                        .master
                        .lock()
                        .map_err(|_| anyhow::anyhow!("PTY master mutex poisoned on reattach"))?;
                    master
                        .try_clone_reader()
                        .map_err(|e| anyhow::anyhow!("clone PTY reader on reattach: {e}"))?
                };

                // Resize to the current browser window.
                {
                    let master = slot
                        .master
                        .lock()
                        .map_err(|_| anyhow::anyhow!("PTY master mutex poisoned on resize"))?;
                    let _ = master.resize(portable_pty::PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }

                slot.attached = false; // will be set true by mark_attached()

                return Ok(PtySession {
                    id: device_id,
                    stdin: Arc::clone(&slot.stdin),
                    reader,
                    child: Arc::clone(&slot.child),
                    master: Arc::clone(&slot.master),
                });
            }

            // Process has exited — remove the stale slot and fall through to
            // spawn a fresh one.
            map.remove(&device_id);
        }

        // No live session — spawn a new one.
        let command: Vec<String> = self
            .default_pty_command
            .iter()
            .map(|s| s.replace("{id}", &device_id.to_string().replace('-', "")[..8]))
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

        map.insert(
            device_id,
            SessionSlot {
                master: Arc::clone(&session.master),
                stdin: Arc::clone(&session.stdin),
                child: Arc::clone(&session.child),
                attached: false,
            },
        );

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
    ///
    /// The underlying process is NOT killed — it will be reused on the next
    /// reconnect if it is still running.
    pub async fn mark_detached(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        if let Some(slot) = map.get_mut(&device_id) {
            slot.attached = false;
        }
    }

    /// Kill a session and remove it from the map.
    pub async fn kill(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        if let Some(slot) = map.remove(&device_id) {
            let _ = slot.child.lock().map(|mut c| c.kill());
        }
    }

    /// Remove a session record without killing the process (e.g. after
    /// natural exit is detected).
    pub async fn remove(&self, device_id: Uuid) {
        let mut map = self.sessions.lock().await;
        map.remove(&device_id);
    }

    /// True if any session record exists for this device.
    pub async fn has_session(&self, device_id: Uuid) -> bool {
        let map = self.sessions.lock().await;
        map.contains_key(&device_id)
    }

    /// True if a session exists **and** the child is still running.
    pub async fn is_alive(&self, device_id: Uuid) -> bool {
        let mut map = self.sessions.lock().await;
        map.get_mut(&device_id)
            .map(|s| s.is_alive())
            .unwrap_or(false)
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
