// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! `SessionSpawner` — the abstraction seam between session management and
//! process execution.
//!
//! # Unix philosophy
//!
//! This trait is the "means not policy" boundary: it says *that* a PTY session
//! can be spawned, not *how* or *where*.  The initial implementation
//! (`LocalSpawner`) forks a process directly.  A future `ContainerSpawner`
//! can wrap Docker/Podman to give each session its own isolated container
//! without changing any of the session management code.
//!
//! # Container groundwork
//!
//! When containerisation is added, `SessionConfig::metadata` carries any
//! spawner-specific state (container image, resource limits, etc.).  The
//! `PtySession::metadata` field carries the resulting opaque state (container
//! ID, etc.) so the manager can clean up containers on session termination.

use std::{collections::HashMap, path::PathBuf};

use async_trait::async_trait;

use super::PtySession;

/// Parameters for spawning a PTY session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Initial terminal width in columns.
    pub cols: u16,
    /// Initial terminal height in rows.
    pub rows: u16,
    /// Working directory for the spawned process.
    pub working_dir: PathBuf,
    /// Environment variables (merged with the node process environment).
    pub env: HashMap<String, String>,
    /// Command to execute inside the PTY.
    ///
    /// Arg 0 is the program; subsequent entries are arguments.
    pub command: Vec<String>,
}

/// Trait for spawning PTY sessions.
///
/// Implement this to add support for containerised sessions, remote
/// execution, or any other PTY-backed execution environment.
#[async_trait]
pub trait SessionSpawner: Send + Sync + 'static {
    /// Spawn a new PTY session with the given configuration.
    ///
    /// Returns a [`PtySession`] whose `master` field is the read/write end
    /// of the PTY.  The session is considered alive until `kill()` is called
    /// or the child process exits.
    async fn spawn(&self, config: SessionConfig) -> anyhow::Result<PtySession>;
}
