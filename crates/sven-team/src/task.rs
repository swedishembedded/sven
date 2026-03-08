// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared task list data model and file-locked JSON storage.
//!
//! # Storage layout
//!
//! ```text
//! ~/.config/sven/teams/{team-name}/tasks.json   ← the task list
//! ```
//!
//! # Concurrency
//!
//! All mutations use an exclusive `flock(2)` on the tasks file, so multiple
//! sven node processes in the same team can safely claim and update tasks
//! without races.  The lock is held only for the duration of a
//! read-modify-write cycle (typically a few microseconds).

use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current state of a team task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TaskStatus {
    /// Waiting to be claimed; all dependencies are resolved.
    Pending,
    /// Claimed by a teammate and actively being worked on.
    InProgress {
        /// Name of the peer that claimed the task.
        claimed_by: String,
        /// When the task was claimed.
        claimed_at: DateTime<Utc>,
    },
    /// Successfully finished.
    Completed {
        /// Summary of what was done; injected into the lead's context.
        summary: String,
        completed_at: DateTime<Utc>,
    },
    /// Finished with an error or could not be completed.
    Failed {
        reason: String,
        failed_at: DateTime<Utc>,
    },
    /// Explicitly cancelled by the lead.
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed { .. } | TaskStatus::Failed { .. } | TaskStatus::Cancelled
        )
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, TaskStatus::Pending)
    }

    pub fn label(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress { .. } => "in_progress",
            TaskStatus::Completed { .. } => "completed",
            TaskStatus::Failed { .. } => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }
}

/// A single work item in the shared task list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier — stable across updates.
    pub id: String,
    /// Short human-readable title (1 line).
    pub title: String,
    /// Full task description with context and success criteria.
    pub description: String,
    /// Current state of the task.
    pub status: TaskStatus,
    /// Peer name the lead has explicitly assigned this to, if any.
    /// When `None` any teammate may self-claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    /// IDs of tasks that must complete before this one can be claimed.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Name of the peer that created this task.
    pub created_by: String,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task last transitioned state.
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// Create a new pending task.
    pub fn new(
        title: impl Into<String>,
        description: impl Into<String>,
        created_by: impl Into<String>,
        depends_on: Vec<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            title: title.into(),
            description: description.into(),
            status: TaskStatus::Pending,
            assigned_to: None,
            depends_on,
            created_by: created_by.into(),
            created_at: now,
            updated_at: now,
        }
    }

    /// Returns `true` when all dependencies are satisfied (completed) in `list`.
    pub fn dependencies_satisfied(&self, list: &TaskList) -> bool {
        self.depends_on.iter().all(|dep_id| {
            list.tasks
                .iter()
                .find(|t| &t.id == dep_id)
                .map(|t| matches!(t.status, TaskStatus::Completed { .. }))
                .unwrap_or(true) // unknown dependency = treat as satisfied
        })
    }
}

/// The full shared task list for a team.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskList {
    pub tasks: Vec<Task>,
}

impl TaskList {
    /// Return a reference to the task with `id`, or `None`.
    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    /// Return a mutable reference to the task with `id`, or `None`.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    /// Return all claimable tasks: pending, unblocked, unassigned or assigned to `peer`.
    pub fn claimable_by<'a>(&'a self, peer: &str) -> Vec<&'a Task> {
        self.tasks
            .iter()
            .filter(|t| {
                t.status.is_pending()
                    && t.dependencies_satisfied(self)
                    && t.assigned_to.as_deref().map(|a| a == peer).unwrap_or(true)
            })
            .collect()
    }

    /// Count tasks by label.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut pending = 0;
        let mut in_progress = 0;
        let mut completed = 0;
        let mut failed = 0;
        for t in &self.tasks {
            match t.status {
                TaskStatus::Pending => pending += 1,
                TaskStatus::InProgress { .. } => in_progress += 1,
                TaskStatus::Completed { .. } => completed += 1,
                TaskStatus::Failed { .. } | TaskStatus::Cancelled => failed += 1,
            }
        }
        (pending, in_progress, completed, failed)
    }
}

// ── File-locked storage ───────────────────────────────────────────────────────

/// Error type for task store operations.
#[derive(Debug, thiserror::Error)]
pub enum TaskStoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("task not found: {0}")]
    NotFound(String),
    #[error("task is not in the expected state: {0}")]
    InvalidState(String),
}

/// File-backed task list store for a specific team.
///
/// All read-modify-write operations acquire an exclusive `flock` on the
/// tasks file so multiple node processes on the same machine can safely
/// share the task list.
pub struct TaskStore {
    path: PathBuf,
}

impl TaskStore {
    /// Open (or create) the store for `team_name` under the default config dir.
    pub fn open(team_name: &str) -> Result<Self, TaskStoreError> {
        let dir = default_team_dir(team_name);
        fs::create_dir_all(&dir)?;
        let path = dir.join("tasks.json");
        // Create the file if it does not exist yet with an empty task list.
        if !path.exists() {
            let initial = serde_json::to_string_pretty(&TaskList::default())?;
            fs::write(&path, initial)?;
        }
        Ok(Self { path })
    }

    /// Open the store at an explicit path (useful for tests).
    pub fn open_at(path: PathBuf) -> Result<Self, TaskStoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            let initial = serde_json::to_string_pretty(&TaskList::default())?;
            fs::write(&path, initial)?;
        }
        Ok(Self { path })
    }

    /// Read the task list from disk (no lock — use for reads only).
    pub fn load(&self) -> Result<TaskList, TaskStoreError> {
        let data = fs::read_to_string(&self.path)?;
        let list: TaskList = serde_json::from_str(&data)?;
        Ok(list)
    }

    /// Perform a locked read-modify-write cycle on the task list.
    ///
    /// `f` receives the current list and may mutate it.  The modified list is
    /// written back atomically on success.  The exclusive flock is held for
    /// the entire duration of `f`.
    pub fn modify<F, R>(&self, f: F) -> Result<R, TaskStoreError>
    where
        F: FnOnce(&mut TaskList) -> Result<R, TaskStoreError>,
    {
        use std::os::unix::io::AsRawFd;

        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.path)?;

        // Acquire exclusive lock (blocking).
        // SAFETY: flock is a valid syscall on the file descriptor.
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if ret != 0 {
            return Err(TaskStoreError::Io(std::io::Error::last_os_error()));
        }

        // Read current content.
        let mut data = String::new();
        let mut reader = std::io::BufReader::new(&file);
        reader.read_to_string(&mut data)?;
        let mut list: TaskList = if data.trim().is_empty() {
            TaskList::default()
        } else {
            serde_json::from_str(&data)?
        };

        // Apply the mutation.
        let result = f(&mut list)?;

        // Write back.
        let serialized = serde_json::to_string_pretty(&list)?;
        // Truncate to zero and write from the beginning.
        file.set_len(0)?;
        let mut writer = std::io::BufWriter::new(&file);
        writer.write_all(serialized.as_bytes())?;
        writer.flush()?;

        // Release lock (happens automatically when `file` drops, but explicit
        // for clarity).
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };

        Ok(result)
    }

    /// Add a new task to the list.  Returns the task ID.
    pub fn create_task(
        &self,
        title: impl Into<String>,
        description: impl Into<String>,
        created_by: impl Into<String>,
        depends_on: Vec<String>,
    ) -> Result<String, TaskStoreError> {
        let task = Task::new(title, description, created_by, depends_on);
        let id = task.id.clone();
        self.modify(|list| {
            list.tasks.push(task.clone());
            Ok(())
        })?;
        Ok(id)
    }

    /// Atomically claim the first available task for `peer`.
    ///
    /// Returns the claimed task, or `None` when no unblocked task is available.
    pub fn claim_next(&self, peer: &str) -> Result<Option<Task>, TaskStoreError> {
        self.modify(|list| {
            // Collect claimable task IDs first to avoid borrow conflicts.
            let claimable_ids: Vec<String> = list
                .claimable_by(peer)
                .into_iter()
                .map(|t| t.id.clone())
                .collect();

            let Some(id) = claimable_ids.into_iter().next() else {
                return Ok(None);
            };

            let task = list
                .get_mut(&id)
                .ok_or_else(|| TaskStoreError::NotFound(id.clone()))?;

            task.status = TaskStatus::InProgress {
                claimed_by: peer.to_string(),
                claimed_at: Utc::now(),
            };
            task.updated_at = Utc::now();
            Ok(Some(task.clone()))
        })
    }

    /// Atomically claim a specific task for `peer`.
    pub fn claim_task(&self, task_id: &str, peer: &str) -> Result<Task, TaskStoreError> {
        self.modify(|list| {
            let task = list
                .get_mut(task_id)
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;

            if !task.status.is_pending() {
                return Err(TaskStoreError::InvalidState(format!(
                    "task {} is {}, not pending",
                    task_id,
                    task.status.label()
                )));
            }

            if let Some(assigned) = &task.assigned_to {
                if assigned != peer {
                    return Err(TaskStoreError::InvalidState(format!(
                        "task {} is assigned to {assigned}, not {peer}",
                        task_id
                    )));
                }
            }

            task.status = TaskStatus::InProgress {
                claimed_by: peer.to_string(),
                claimed_at: Utc::now(),
            };
            task.updated_at = Utc::now();
            Ok(task.clone())
        })
    }

    /// Mark a task as completed with a summary.
    pub fn complete_task(
        &self,
        task_id: &str,
        summary: impl Into<String>,
    ) -> Result<(), TaskStoreError> {
        self.modify(|list| {
            let task = list
                .get_mut(task_id)
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;

            task.status = TaskStatus::Completed {
                summary: summary.into(),
                completed_at: Utc::now(),
            };
            task.updated_at = Utc::now();
            Ok(())
        })
    }

    /// Mark a task as failed.
    pub fn fail_task(
        &self,
        task_id: &str,
        reason: impl Into<String>,
    ) -> Result<(), TaskStoreError> {
        self.modify(|list| {
            let task = list
                .get_mut(task_id)
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;

            task.status = TaskStatus::Failed {
                reason: reason.into(),
                failed_at: Utc::now(),
            };
            task.updated_at = Utc::now();
            Ok(())
        })
    }

    /// Assign a task to a specific peer.
    pub fn assign_task(
        &self,
        task_id: &str,
        assignee: impl Into<String>,
    ) -> Result<(), TaskStoreError> {
        self.modify(|list| {
            let task = list
                .get_mut(task_id)
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;

            task.assigned_to = Some(assignee.into());
            task.updated_at = Utc::now();
            Ok(())
        })
    }

    /// Update the description of a task (while keeping its current status).
    pub fn update_description(
        &self,
        task_id: &str,
        description: impl Into<String>,
    ) -> Result<(), TaskStoreError> {
        self.modify(|list| {
            let task = list
                .get_mut(task_id)
                .ok_or_else(|| TaskStoreError::NotFound(task_id.to_string()))?;

            task.description = description.into();
            task.updated_at = Utc::now();
            Ok(())
        })
    }
}

/// Compute the default directory for a team's data.
pub fn default_team_dir(team_name: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("sven")
        .join("teams")
        .join(team_name)
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, TaskStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tasks.json");
        let store = TaskStore::open_at(path).unwrap();
        (dir, store)
    }

    #[test]
    fn create_and_load() {
        let (_dir, s) = store();
        let id = s
            .create_task("Do X", "Detailed desc", "alice", vec![])
            .unwrap();
        let list = s.load().unwrap();
        assert_eq!(list.tasks.len(), 1);
        assert_eq!(list.tasks[0].id, id);
        assert!(list.tasks[0].status.is_pending());
    }

    #[test]
    fn claim_next_marks_in_progress() {
        let (_dir, s) = store();
        s.create_task("T1", "desc", "alice", vec![]).unwrap();
        let claimed = s.claim_next("bob").unwrap().expect("should claim");
        assert!(
            matches!(claimed.status, TaskStatus::InProgress { claimed_by, .. } if claimed_by == "bob")
        );
    }

    #[test]
    fn complete_task_marks_completed() {
        let (_dir, s) = store();
        let id = s.create_task("T1", "desc", "alice", vec![]).unwrap();
        s.claim_task(&id, "bob").unwrap();
        s.complete_task(&id, "Done").unwrap();
        let list = s.load().unwrap();
        assert!(matches!(list.tasks[0].status, TaskStatus::Completed { .. }));
    }

    #[test]
    fn assign_task_sets_assignee() {
        let (_dir, s) = store();
        let id = s.create_task("T1", "desc", "alice", vec![]).unwrap();
        s.assign_task(&id, "carol").unwrap();
        let list = s.load().unwrap();
        assert_eq!(list.tasks[0].assigned_to.as_deref(), Some("carol"));
    }

    #[test]
    fn claim_next_respects_assignment() {
        let (_dir, s) = store();
        let id = s.create_task("T1", "desc", "alice", vec![]).unwrap();
        s.assign_task(&id, "carol").unwrap();
        // bob cannot claim carol's task
        let not_claimed = s.claim_next("bob").unwrap();
        assert!(not_claimed.is_none());
        // carol can
        let claimed = s.claim_next("carol").unwrap();
        assert!(claimed.is_some());
    }

    #[test]
    fn dependency_blocks_claim() {
        let (_dir, s) = store();
        let dep_id = s
            .create_task("T0", "prerequisite", "alice", vec![])
            .unwrap();
        let _id = s
            .create_task("T1", "depends on T0", "alice", vec![dep_id.clone()])
            .unwrap();
        // T1 is blocked; only T0 can be claimed.
        let claimed = s.claim_next("bob").unwrap().expect("should claim T0");
        assert_eq!(claimed.title, "T0");
    }

    #[test]
    fn dependency_unblocks_after_completion() {
        let (_dir, s) = store();
        let dep_id = s
            .create_task("T0", "prerequisite", "alice", vec![])
            .unwrap();
        let blocked_id = s
            .create_task("T1", "depends on T0", "alice", vec![dep_id.clone()])
            .unwrap();
        s.claim_task(&dep_id, "bob").unwrap();
        s.complete_task(&dep_id, "done").unwrap();
        // Now T1 should be claimable.
        let claimed = s.claim_task(&blocked_id, "carol").unwrap();
        assert_eq!(claimed.title, "T1");
    }
}
