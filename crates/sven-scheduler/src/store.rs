// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! YAML-backed persistent job store.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use tokio::sync::Mutex;
use tracing::{debug, info};

use crate::job::{Job, JobId};

/// Persistent store for scheduled jobs.
///
/// Jobs are serialised to a YAML file and reloaded on startup.
/// All mutations are immediately flushed to disk.
#[derive(Clone)]
pub struct JobStore {
    jobs: Arc<Mutex<Vec<Job>>>,
    path: Option<PathBuf>,
}

impl JobStore {
    /// Load jobs from `path` (or default location) if the file exists.
    ///
    /// If the file does not exist an empty store is returned; the file
    /// will be created when the first job is added.
    pub fn load_or_default(path: Option<&Path>) -> anyhow::Result<Self> {
        let resolved = path
            .map(|p| p.to_path_buf())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config/sven/scheduler/jobs.yaml")));

        let jobs = match &resolved {
            Some(p) if p.is_file() => {
                let text = std::fs::read_to_string(p)?;
                let loaded: Vec<Job> = serde_yaml::from_str(&text).unwrap_or_default();
                info!(path = %p.display(), count = loaded.len(), "loaded scheduler jobs");
                loaded
            }
            _ => Vec::new(),
        };

        Ok(Self {
            jobs: Arc::new(Mutex::new(jobs)),
            path: resolved,
        })
    }

    /// Add a new job to the store and persist.
    pub async fn add(&self, job: Job) -> anyhow::Result<JobId> {
        let id = job.id;
        self.jobs.lock().await.push(job);
        self.flush().await?;
        Ok(id)
    }

    /// Remove a job by ID.  Returns `true` if found and removed.
    pub async fn remove(&self, id: JobId) -> anyhow::Result<bool> {
        let mut jobs = self.jobs.lock().await;
        let before = jobs.len();
        jobs.retain(|j| j.id != id);
        let removed = jobs.len() < before;
        drop(jobs);
        if removed {
            self.flush().await?;
        }
        Ok(removed)
    }

    /// Enable or disable a job by ID.
    pub async fn set_enabled(&self, id: JobId, enabled: bool) -> anyhow::Result<bool> {
        let mut jobs = self.jobs.lock().await;
        let found = jobs.iter_mut().find(|j| j.id == id);
        if let Some(job) = found {
            job.enabled = enabled;
            drop(jobs);
            self.flush().await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Update a job after execution (advances `last_run` and `next_run`).
    pub async fn advance(&self, id: JobId) -> anyhow::Result<()> {
        let mut jobs = self.jobs.lock().await;
        if let Some(job) = jobs.iter_mut().find(|j| j.id == id) {
            job.advance();
        }
        drop(jobs);
        self.flush().await
    }

    /// Return a snapshot of all jobs.
    pub async fn list(&self) -> Vec<Job> {
        self.jobs.lock().await.clone()
    }

    /// Return all enabled jobs whose `next_run` is at or before `now`.
    pub async fn due_jobs(&self) -> Vec<Job> {
        let now = chrono::Utc::now();
        self.jobs
            .lock()
            .await
            .iter()
            .filter(|j| j.enabled)
            .filter(|j| j.next_run.map(|t| t <= now).unwrap_or(false))
            .cloned()
            .collect()
    }

    async fn flush(&self) -> anyhow::Result<()> {
        let path = match &self.path {
            Some(p) => p.clone(),
            None => {
                debug!("JobStore: no path configured, skipping flush");
                return Ok(());
            }
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let jobs = self.jobs.lock().await.clone();
        let yaml = serde_yaml::to_string(&jobs)?;
        tokio::fs::write(&path, yaml).await?;
        debug!(path = %path.display(), "JobStore: flushed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, Schedule};

    #[tokio::test]
    async fn add_and_list() {
        let store = JobStore::load_or_default(None).unwrap();
        let job = Job::new(
            "test-job",
            Schedule::Interval {
                every: "1h".to_string(),
            },
            "do something",
        );
        let id = store.add(job).await.unwrap();
        let jobs = store.list().await;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, id);
    }

    #[tokio::test]
    async fn remove_existing() {
        let store = JobStore::load_or_default(None).unwrap();
        let job = Job::new(
            "to-remove",
            Schedule::Interval {
                every: "1h".to_string(),
            },
            "prompt",
        );
        let id = store.add(job).await.unwrap();
        assert!(store.remove(id).await.unwrap());
        assert!(store.list().await.is_empty());
    }

    #[tokio::test]
    async fn remove_nonexistent() {
        let store = JobStore::load_or_default(None).unwrap();
        let id = uuid::Uuid::new_v4();
        assert!(!store.remove(id).await.unwrap());
    }
}
