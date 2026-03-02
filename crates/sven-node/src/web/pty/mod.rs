// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! PTY session management for the web terminal.
//!
//! Each approved browser device gets one `PtySession` — a server-side PTY
//! process (default: tmux) that persists across browser reconnects.

pub mod local;
pub mod manager;
pub mod spawner;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use portable_pty::MasterPty;
use uuid::Uuid;

/// A live server-side PTY session.
///
/// Fields are separated so they can be moved into different tasks:
/// - `reader` → blocking read task (PTY output → WebSocket)
/// - `stdin`  → WebSocket receive task (keyboard input → PTY)
/// - `master` → shared between resize calls
pub struct PtySession {
    /// Stable session identifier (= device UUID).
    pub id: Uuid,
    /// Write half of the PTY master — send bytes to the process's stdin.
    pub stdin: Box<dyn Write + Send>,
    /// Blocking reader for terminal output; handed to the WebSocket bridge.
    pub reader: Box<dyn std::io::Read + Send>,
    /// Handle to the child process for cleanup.
    pub child: Box<dyn portable_pty::Child + Send + Sync>,
    /// PTY master (kept alive for resize operations).
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}

impl PtySession {
    /// Resize the terminal.  `cols` × `rows` are in character cells.
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        let master = self
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY master mutex poisoned"))?;
        master
            .resize(portable_pty::PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| anyhow::anyhow!("PTY resize failed: {e}"))
    }

    /// Send bytes to the PTY master (keyboard input from the browser).
    pub fn write_input(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stdin.write_all(data)
    }

    /// Kill the child process.
    pub fn kill(&mut self) -> anyhow::Result<()> {
        self.child
            .kill()
            .map_err(|e| anyhow::anyhow!("PTY kill failed: {e}"))
    }
}
