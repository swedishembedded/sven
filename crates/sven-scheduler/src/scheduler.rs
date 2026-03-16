// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Scheduler — polls job store every minute and emits due-job events.

use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use crate::store::JobStore;

/// Event emitted when a scheduled job is due to run.
#[derive(Debug, Clone)]
pub struct JobDue {
    /// Identifier of the job that fired.
    pub job_id: Uuid,
    /// Job name for logging.
    pub job_name: String,
    /// Prompt to send to the agent.
    pub prompt: String,
    /// Optional delivery target (`"channel:recipient"`).
    pub deliver_to: Option<String>,
    /// Whether to run in an isolated session.
    pub isolated: bool,
}

/// Background scheduler that polls for due jobs and emits events.
///
/// The scheduler checks the job store every 30 seconds and sends a
/// [`JobDue`] event for each job whose `next_run` has passed.
#[derive(Clone)]
pub struct Scheduler {
    store: Arc<JobStore>,
    tx: mpsc::Sender<JobDue>,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(store: Arc<JobStore>, tx: mpsc::Sender<JobDue>) -> Self {
        Self { store, tx }
    }

    /// Spawn the scheduler loop as a background tokio task.
    ///
    /// Returns immediately. The task runs until the [`mpsc::Sender`] is dropped.
    pub async fn start(&self) {
        let store = self.store.clone();
        let tx = self.tx.clone();

        info!("Scheduler starting");

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));

            loop {
                interval.tick().await;

                let due = store.due_jobs().await;
                for job in due {
                    debug!(
                        job_id = %job.id,
                        job_name = %job.name,
                        "Scheduler: job due"
                    );

                    let event = JobDue {
                        job_id: job.id,
                        job_name: job.name.clone(),
                        prompt: job.prompt.clone(),
                        deliver_to: job.deliver_to.clone(),
                        isolated: job.isolated,
                    };

                    if tx.send(event).await.is_err() {
                        info!("Scheduler: receiver dropped — stopping");
                        return;
                    }

                    // Advance the schedule so the job won't fire again immediately
                    let _ = store.advance(job.id).await;
                }
            }
        });
    }
}
