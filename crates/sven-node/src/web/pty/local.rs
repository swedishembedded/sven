// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! Local PTY spawner — runs the session command in the current process tree
//! using `portable-pty`.

use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use async_trait::async_trait;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tracing::info;

use super::{
    spawner::{SessionConfig, SessionSpawner},
    PtySession,
};

/// Spawner that runs commands in a local PTY within the same process tree.
pub struct LocalSpawner;

#[async_trait]
impl SessionSpawner for LocalSpawner {
    async fn spawn(&self, config: SessionConfig) -> anyhow::Result<PtySession> {
        tokio::task::spawn_blocking(move || spawn_local(config))
            .await
            .map_err(|e| anyhow::anyhow!("spawn task panicked: {e}"))?
    }
}

fn spawn_local(config: SessionConfig) -> anyhow::Result<PtySession> {
    if config.command.is_empty() {
        anyhow::bail!("PTY command must not be empty");
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("openpty failed")?;

    let mut cmd = CommandBuilder::new(&config.command[0]);
    for arg in &config.command[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(&config.working_dir);
    for (k, v) in &config.env {
        cmd.env(k, v);
    }

    info!(
        command = %config.command.join(" "),
        cwd = %config.working_dir.display(),
        cols = config.cols,
        rows = config.rows,
        "spawning local PTY session"
    );

    let child = pair
        .slave
        .spawn_command(cmd)
        .context("spawning PTY command")?;

    // Obtain reader (cloned from master — does not consume the master).
    let reader = pair.master.try_clone_reader().context("clone PTY reader")?;

    // Obtain writer from master and wrap in Arc<Mutex> for session persistence.
    let stdin = Arc::new(Mutex::new(
        pair.master
            .take_writer()
            .context("take PTY writer")
            .map(|w| -> Box<dyn std::io::Write + Send> { w })?,
    ));

    // Wrap the master in Arc<Mutex> so resize and reader-clone can share it.
    let master = Arc::new(Mutex::new(pair.master));

    // Wrap child in Arc<Mutex> for exit detection across reconnects.
    let child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>> = Arc::new(Mutex::new(child));

    Ok(PtySession {
        id: uuid::Uuid::nil(), // filled in by manager
        stdin,
        reader,
        child,
        master,
    })
}
