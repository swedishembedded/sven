// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Heartbeat — periodic agent wakeup at a configurable interval.
//!
//! The heartbeat is separate from the cron scheduler: it wakes the main
//! agent session on a fixed interval using the configured prompt. If a
//! `HEARTBEAT.md` file exists in the workspace root, its contents are
//! appended to the heartbeat prompt as standing instructions.

use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::scheduler::JobDue;

/// Heartbeat configuration.
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// Whether the heartbeat is active.
    pub enabled: bool,
    /// Interval string (e.g. `"30m"`, `"1h"`).
    pub every: String,
    /// Base prompt sent to the agent on each beat.
    pub prompt: String,
    /// Absolute path to the workspace root, used to find `HEARTBEAT.md`.
    pub workspace_root: Option<std::path::PathBuf>,
}

/// Background task that fires a [`JobDue`] event at the configured interval.
pub struct Heartbeat {
    config: HeartbeatConfig,
    tx: mpsc::Sender<JobDue>,
}

impl Heartbeat {
    /// Create a new heartbeat.
    pub fn new(config: HeartbeatConfig, tx: mpsc::Sender<JobDue>) -> Self {
        Self { config, tx }
    }

    /// Spawn the heartbeat loop as a background tokio task.
    ///
    /// Returns immediately; the task continues until the sender is dropped.
    pub async fn start(self) {
        if !self.config.enabled {
            debug!("Heartbeat disabled — not starting");
            return;
        }

        let dur = match humantime::parse_duration(&self.config.every) {
            Ok(d) => d,
            Err(e) => {
                tracing::error!(
                    error = %e,
                    every = %self.config.every,
                    "Heartbeat: invalid interval — disabling"
                );
                return;
            }
        };

        info!(every = %self.config.every, "Heartbeat starting");

        let prompt = self.config.prompt.clone();
        let workspace_root = self.config.workspace_root.clone();
        let tx = self.tx;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(dur);
            interval.tick().await; // skip first immediate tick

            loop {
                interval.tick().await;

                // Append HEARTBEAT.md standing instructions if present
                let full_prompt = build_heartbeat_prompt(&prompt, workspace_root.as_deref()).await;

                debug!("Heartbeat: firing");

                let event = JobDue {
                    job_id: uuid::Uuid::nil(),
                    job_name: "heartbeat".to_string(),
                    prompt: full_prompt,
                    deliver_to: None,
                    isolated: false,
                };

                if tx.send(event).await.is_err() {
                    info!("Heartbeat: receiver dropped — stopping");
                    return;
                }
            }
        });
    }
}

async fn build_heartbeat_prompt(base: &str, workspace: Option<&std::path::Path>) -> String {
    let standing = workspace
        .map(|root| root.join("HEARTBEAT.md"))
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(p).ok());

    match standing {
        Some(instructions) if !instructions.trim().is_empty() => {
            format!(
                "{base}\n\n## Standing Instructions\n\n{}",
                instructions.trim()
            )
        }
        _ => base.to_string(),
    }
}
