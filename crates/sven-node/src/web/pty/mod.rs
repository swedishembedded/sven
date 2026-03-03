// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! PTY session management for the web terminal.
//!
//! Each approved browser device gets one `PtySession`.  Sessions persist
//! across browser reconnects: the underlying process stays alive while the
//! WebSocket is temporarily closed, and the browser reattaches by cloning a
//! new reader from the master PTY without restarting the process.

pub mod local;
pub mod manager;
pub mod spawner;

use std::sync::{Arc, Mutex};

use portable_pty::MasterPty;
use uuid::Uuid;

/// A live server-side PTY session.
///
/// `stdin` and `child` are `Arc<Mutex<…>>` so the `PtyManager` can hold a
/// reference in the session slot while the current WebSocket handler also
/// uses them.  When a WebSocket closes, the slot's references keep both alive
/// so the next reconnect can reattach without restarting the process.
pub struct PtySession {
    /// Stable session identifier (= device UUID).
    pub id: Uuid,
    /// Write half of the PTY master.  Shared with the slot so it survives
    /// WebSocket disconnects.
    pub stdin: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    /// Fresh blocking reader for this WebSocket connection's output stream.
    /// Each reconnect gets a new reader cloned from the master.
    pub reader: Box<dyn std::io::Read + Send>,
    /// Child process handle.  Shared with the slot for exit detection and
    /// cleanup; kept alive as long as the slot or session holds a reference.
    pub child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    /// PTY master (kept alive for resize operations and reader cloning).
    pub master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
}
