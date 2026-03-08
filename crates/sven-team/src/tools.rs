// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent tools for the shared team task list.
//!
//! These tools are registered in the node agent when it is part of a team.
//! The LLM can use them to create, claim, complete, and list tasks.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

use crate::task::{Task, TaskStatus, TaskStore};

// ── Shared handle ─────────────────────────────────────────────────────────────

/// Thread-safe handle to the team task store.
pub type TaskStoreHandle = Arc<Mutex<TaskStore>>;

// ── CreateTaskTool ───────────────────────────────────────────────────────────

/// Create a new task in the team's shared task list.
pub struct CreateTaskTool {
    pub store: TaskStoreHandle,
    /// Name of the agent creating the task (used as `created_by`).
    pub agent_name: String,
}

#[async_trait]
impl Tool for CreateTaskTool {
    fn name(&self) -> &str {
        "create_task"
    }

    fn description(&self) -> &str {
        "Create a new task in the team's shared task list. \
         Each task should be a self-contained unit of work that produces a clear deliverable. \
         Tasks can depend on other tasks — a dependent task cannot be claimed until all its \
         dependencies are completed. \
         Returns the new task ID."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["title", "description"],
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short task title (1 line, < 80 chars)"
                },
                "description": {
                    "type": "string",
                    "description": "Full task description: what must be done, success criteria, context"
                },
                "assigned_to": {
                    "type": "string",
                    "description": "Optional: agent name to assign this task to. Omit to allow any teammate to self-claim."
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional: list of task IDs that must be completed before this task can be claimed."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let title = match call.args["title"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: title"),
        };
        let description = match call.args["description"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: description"),
        };
        let assigned_to = call.args["assigned_to"].as_str().map(|s| s.to_string());
        let depends_on: Vec<String> = call.args["depends_on"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let store = self.store.lock().await;
        match store.create_task(title.clone(), description, &self.agent_name, depends_on) {
            Ok(id) => {
                if let Some(assignee) = assigned_to {
                    let _ = store.assign_task(&id, &assignee);
                }
                ToolOutput::ok(
                    &call.id,
                    format!("Task created: \"{title}\"\nTask ID: {id}\nStatus: pending"),
                )
            }
            Err(e) => ToolOutput::err(&call.id, format!("Failed to create task: {e}")),
        }
    }
}

// ── ClaimTaskTool ─────────────────────────────────────────────────────────────

/// Claim a pending task from the shared task list.
pub struct ClaimTaskTool {
    pub store: TaskStoreHandle,
    /// Name of the agent claiming the task.
    pub agent_name: String,
}

#[async_trait]
impl Tool for ClaimTaskTool {
    fn name(&self) -> &str {
        "claim_task"
    }

    fn description(&self) -> &str {
        "Claim a pending task from the team's shared task list. \
         Atomically marks the task as in-progress so other teammates won't claim it. \
         You can either claim a specific task by ID, or claim the next available task. \
         Blocked tasks (with unmet dependencies) cannot be claimed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the specific task to claim. Omit to claim the next available task."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let store = self.store.lock().await;

        if let Some(id) = call.args["task_id"].as_str().filter(|s| !s.is_empty()) {
            match store.claim_task(id, &self.agent_name) {
                Ok(task) => ToolOutput::ok(
                    &call.id,
                    format!(
                        "Claimed task: \"{}\"\nTask ID: {}\nDescription: {}",
                        task.title, task.id, task.description
                    ),
                ),
                Err(e) => ToolOutput::err(&call.id, format!("Failed to claim task {id}: {e}")),
            }
        } else {
            match store.claim_next(&self.agent_name) {
                Ok(Some(task)) => ToolOutput::ok(
                    &call.id,
                    format!(
                        "Claimed task: \"{}\"\nTask ID: {}\nDescription: {}",
                        task.title, task.id, task.description
                    ),
                ),
                Ok(None) => ToolOutput::ok(
                    &call.id,
                    "No pending tasks available. All tasks are either in progress, completed, or blocked by dependencies.",
                ),
                Err(e) => ToolOutput::err(&call.id, format!("Failed to claim task: {e}")),
            }
        }
    }
}

// ── CompleteTaskTool ──────────────────────────────────────────────────────────

/// Mark a task as completed with a summary.
pub struct CompleteTaskTool {
    pub store: TaskStoreHandle,
}

#[async_trait]
impl Tool for CompleteTaskTool {
    fn name(&self) -> &str {
        "complete_task"
    }

    fn description(&self) -> &str {
        "Mark a task as completed with a summary of what was accomplished. \
         The summary is shown in list_tasks output and helps the lead synthesize results. \
         Completing a task unblocks any dependent tasks automatically."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id", "summary"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to mark as completed"
                },
                "summary": {
                    "type": "string",
                    "description": "Summary of what was accomplished. Include key findings, changes made, and any caveats."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let task_id = match call.args["task_id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: task_id"),
        };
        let summary = match call.args["summary"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: summary"),
        };

        let store = self.store.lock().await;
        match store.complete_task(&task_id, &summary) {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!("Task {task_id} marked as completed.\nSummary: {summary}"),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to complete task: {e}")),
        }
    }
}

// ── ListTasksTool ─────────────────────────────────────────────────────────────

/// Show all tasks in the team's shared task list.
pub struct ListTasksTool {
    pub store: TaskStoreHandle,
}

#[async_trait]
impl Tool for ListTasksTool {
    fn name(&self) -> &str {
        "list_tasks"
    }

    fn description(&self) -> &str {
        "Show all tasks in the team's shared task list with their current status. \
         Use this to monitor team progress, find blocked tasks, and see what needs synthesis."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status_filter": {
                    "type": "string",
                    "enum": ["all", "pending", "in_progress", "completed", "failed"],
                    "description": "Filter by status (default: all)",
                    "default": "all"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let filter = call.args["status_filter"].as_str().unwrap_or("all");

        let store = self.store.lock().await;
        let list = match store.load() {
            Ok(l) => l,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to load tasks: {e}")),
        };

        let tasks: Vec<&Task> = list
            .tasks
            .iter()
            .filter(|t| match filter {
                "pending" => t.status.is_pending(),
                "in_progress" => matches!(t.status, TaskStatus::InProgress { .. }),
                "completed" => matches!(t.status, TaskStatus::Completed { .. }),
                "failed" => matches!(t.status, TaskStatus::Failed { .. } | TaskStatus::Cancelled),
                _ => true,
            })
            .collect();

        if tasks.is_empty() {
            let (p, i, c, f) = list.counts();
            return ToolOutput::ok(
                &call.id,
                format!(
                    "No tasks match filter '{filter}'. Total: pending={p}, in_progress={i}, completed={c}, failed={f}"
                ),
            );
        }

        let (p, i, c, f) = list.counts();
        let mut lines = vec![format!(
            "Team tasks ({} total: pending={p}, in_progress={i}, completed={c}, failed={f}):\n",
            list.tasks.len()
        )];

        for task in &tasks {
            let status_icon = match &task.status {
                TaskStatus::Pending => "○",
                TaskStatus::InProgress { .. } => "●",
                TaskStatus::Completed { .. } => "✓",
                TaskStatus::Failed { .. } | TaskStatus::Cancelled => "✗",
            };

            let claimed_by = match &task.status {
                TaskStatus::InProgress { claimed_by, .. } => format!(" [{claimed_by}]"),
                _ => String::new(),
            };

            let assigned = task
                .assigned_to
                .as_deref()
                .map(|a| format!(" (assigned: {a})"))
                .unwrap_or_default();

            let blocked = if task.status.is_pending() && !task.depends_on.is_empty() {
                let deps_done = task
                    .depends_on
                    .iter()
                    .filter(|dep_id| {
                        list.get(dep_id)
                            .map(|t| matches!(t.status, TaskStatus::Completed { .. }))
                            .unwrap_or(true)
                    })
                    .count();
                if deps_done < task.depends_on.len() {
                    format!(
                        " [blocked: {}/{} deps done]",
                        deps_done,
                        task.depends_on.len()
                    )
                } else {
                    String::new()
                }
            } else {
                String::new()
            };

            lines.push(format!(
                "{status_icon} [{}] {}{claimed_by}{assigned}{blocked}\n  {}",
                task.id, task.title, task.description
            ));

            if let TaskStatus::Completed { summary, .. } = &task.status {
                lines.push(format!("  Summary: {summary}"));
            }
            if let TaskStatus::Failed { reason, .. } = &task.status {
                lines.push(format!("  Reason: {reason}"));
            }
            lines.push(String::new());
        }

        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

// ── AssignTaskTool ─────────────────────────────────────────────────────────────

/// Assign a task to a specific teammate.
pub struct AssignTaskTool {
    pub store: TaskStoreHandle,
}

#[async_trait]
impl Tool for AssignTaskTool {
    fn name(&self) -> &str {
        "assign_task"
    }

    fn description(&self) -> &str {
        "Assign a pending task to a specific teammate. \
         Only the team lead should use this tool. \
         Unassigned tasks can be self-claimed by any teammate; \
         assigning restricts the task to the specified agent."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id", "assignee"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to assign"
                },
                "assignee": {
                    "type": "string",
                    "description": "Name of the agent to assign the task to"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let task_id = match call.args["task_id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: task_id"),
        };
        let assignee = match call.args["assignee"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: assignee"),
        };

        let store = self.store.lock().await;
        match store.assign_task(&task_id, &assignee) {
            Ok(()) => ToolOutput::ok(
                &call.id,
                format!("Task {task_id} assigned to '{assignee}'."),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to assign task: {e}")),
        }
    }
}

// ── UpdateTaskTool ─────────────────────────────────────────────────────────────

/// Update the description of a task.
pub struct UpdateTaskTool {
    pub store: TaskStoreHandle,
}

#[async_trait]
impl Tool for UpdateTaskTool {
    fn name(&self) -> &str {
        "update_task"
    }

    fn description(&self) -> &str {
        "Update the description of an existing task. \
         Use this to add context, clarify success criteria, or correct an earlier task definition."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["task_id", "description"],
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "ID of the task to update"
                },
                "description": {
                    "type": "string",
                    "description": "New description for the task"
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let task_id = match call.args["task_id"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: task_id"),
        };
        let description = match call.args["description"].as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolOutput::err(&call.id, "Missing required parameter: description"),
        };

        let store = self.store.lock().await;
        match store.update_description(&task_id, &description) {
            Ok(()) => ToolOutput::ok(&call.id, format!("Task {task_id} description updated.")),
            Err(e) => ToolOutput::err(&call.id, format!("Failed to update task: {e}")),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use sven_tools::{Tool, ToolCall};

    use crate::task::TaskStore;

    use super::*;

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: String::new(),
            args,
        }
    }

    fn store_handle(dir: &TempDir) -> TaskStoreHandle {
        let path = dir.path().join("tasks.json");
        Arc::new(Mutex::new(TaskStore::open_at(path).unwrap()))
    }

    // ── Tool metadata (smoke tests) ───────────────────────────────────────────

    #[test]
    fn tool_names_are_stable() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        assert_eq!(
            CreateTaskTool {
                store: h.clone(),
                agent_name: "a".into()
            }
            .name(),
            "create_task"
        );
        assert_eq!(
            ClaimTaskTool {
                store: h.clone(),
                agent_name: "a".into()
            }
            .name(),
            "claim_task"
        );
        assert_eq!(
            CompleteTaskTool { store: h.clone() }.name(),
            "complete_task"
        );
        assert_eq!(ListTasksTool { store: h.clone() }.name(), "list_tasks");
        assert_eq!(AssignTaskTool { store: h.clone() }.name(), "assign_task");
        assert_eq!(UpdateTaskTool { store: h.clone() }.name(), "update_task");
    }

    #[test]
    fn all_tools_have_auto_policy() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        use sven_tools::ApprovalPolicy;
        assert_eq!(
            CreateTaskTool {
                store: h.clone(),
                agent_name: "a".into()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            ClaimTaskTool {
                store: h.clone(),
                agent_name: "a".into()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            CompleteTaskTool { store: h.clone() }.default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            ListTasksTool { store: h.clone() }.default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            AssignTaskTool { store: h.clone() }.default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            UpdateTaskTool { store: h.clone() }.default_policy(),
            ApprovalPolicy::Auto
        );
    }

    // ── CreateTaskTool ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn create_task_missing_title_is_error() {
        let dir = TempDir::new().unwrap();
        let tool = CreateTaskTool {
            store: store_handle(&dir),
            agent_name: "alice".into(),
        };
        let out = tool
            .execute(&call("c1", json!({ "description": "do it" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("title"));
    }

    #[tokio::test]
    async fn create_task_missing_description_is_error() {
        let dir = TempDir::new().unwrap();
        let tool = CreateTaskTool {
            store: store_handle(&dir),
            agent_name: "alice".into(),
        };
        let out = tool.execute(&call("c1", json!({ "title": "T" }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("description"));
    }

    #[tokio::test]
    async fn create_task_success_returns_id() {
        let dir = TempDir::new().unwrap();
        let tool = CreateTaskTool {
            store: store_handle(&dir),
            agent_name: "alice".into(),
        };
        let out = tool
            .execute(&call(
                "c1",
                json!({ "title": "Do X", "description": "Details" }),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("Do X"));
    }

    #[tokio::test]
    async fn create_task_with_assignment() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let tool = CreateTaskTool {
            store: h.clone(),
            agent_name: "alice".into(),
        };
        let out = tool
            .execute(&call(
                "c1",
                json!({
                    "title": "Do X",
                    "description": "Details",
                    "assigned_to": "bob"
                }),
            ))
            .await;
        assert!(!out.is_error);
        let store = h.lock().await;
        let list = store.load().unwrap();
        assert_eq!(list.tasks[0].assigned_to.as_deref(), Some("bob"));
    }

    // ── ClaimTaskTool ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn claim_task_next_available() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        {
            let s = h.lock().await;
            s.create_task("T1", "desc", "alice", vec![]).unwrap();
        }
        let tool = ClaimTaskTool {
            store: h.clone(),
            agent_name: "bob".into(),
        };
        let out = tool.execute(&call("cl1", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("T1"));
    }

    #[tokio::test]
    async fn claim_task_no_tasks_returns_ok_message() {
        let dir = TempDir::new().unwrap();
        let tool = ClaimTaskTool {
            store: store_handle(&dir),
            agent_name: "bob".into(),
        };
        let out = tool.execute(&call("cl1", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No pending tasks"));
    }

    #[tokio::test]
    async fn claim_task_by_id() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let task_id = {
            let s = h.lock().await;
            s.create_task("T1", "desc", "alice", vec![]).unwrap()
        };
        let tool = ClaimTaskTool {
            store: h.clone(),
            agent_name: "bob".into(),
        };
        let out = tool
            .execute(&call("cl1", json!({ "task_id": task_id })))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("T1"));
    }

    // ── CompleteTaskTool ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn complete_task_missing_task_id_is_error() {
        let dir = TempDir::new().unwrap();
        let tool = CompleteTaskTool {
            store: store_handle(&dir),
        };
        let out = tool
            .execute(&call("ct1", json!({ "summary": "done" })))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn complete_task_success() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let task_id = {
            let s = h.lock().await;
            let id = s.create_task("T1", "desc", "alice", vec![]).unwrap();
            s.claim_task(&id, "bob").unwrap();
            id
        };
        let tool = CompleteTaskTool { store: h.clone() };
        let out = tool
            .execute(&call(
                "ct1",
                json!({ "task_id": task_id, "summary": "Done!" }),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("completed"));
    }

    // ── ListTasksTool ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_tasks_empty_store() {
        let dir = TempDir::new().unwrap();
        let tool = ListTasksTool {
            store: store_handle(&dir),
        };
        let out = tool.execute(&call("lt1", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No tasks"));
    }

    #[tokio::test]
    async fn list_tasks_shows_all_by_default() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        {
            let s = h.lock().await;
            s.create_task("Alpha", "desc", "alice", vec![]).unwrap();
            s.create_task("Beta", "desc", "alice", vec![]).unwrap();
        }
        let tool = ListTasksTool { store: h.clone() };
        let out = tool.execute(&call("lt1", json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("Alpha"));
        assert!(out.content.contains("Beta"));
    }

    #[tokio::test]
    async fn list_tasks_filter_pending_only() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let done_id = {
            let s = h.lock().await;
            s.create_task("Done task", "desc", "alice", vec![]).unwrap();
            let id = s
                .create_task("Pending task", "desc", "alice", vec![])
                .unwrap();
            let done = s
                .create_task("Completed task", "desc", "alice", vec![])
                .unwrap();
            s.claim_task(&done, "bob").unwrap();
            s.complete_task(&done, "finished").unwrap();
            id
        };
        let tool = ListTasksTool { store: h.clone() };
        let out = tool
            .execute(&call("lt1", json!({ "status_filter": "pending" })))
            .await;
        assert!(!out.is_error);
        // Completed task should not appear
        assert!(!out.content.contains("Completed task"));
        let _ = done_id;
    }

    // ── AssignTaskTool ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn assign_task_missing_params_is_error() {
        let dir = TempDir::new().unwrap();
        let tool = AssignTaskTool {
            store: store_handle(&dir),
        };
        let out = tool.execute(&call("at1", json!({ "task_id": "x" }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("assignee"));
    }

    #[tokio::test]
    async fn assign_task_success() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let task_id = {
            let s = h.lock().await;
            s.create_task("T1", "desc", "alice", vec![]).unwrap()
        };
        let tool = AssignTaskTool { store: h.clone() };
        let out = tool
            .execute(&call(
                "at1",
                json!({ "task_id": task_id, "assignee": "carol" }),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("carol"));
    }

    // ── UpdateTaskTool ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_task_missing_params_is_error() {
        let dir = TempDir::new().unwrap();
        let tool = UpdateTaskTool {
            store: store_handle(&dir),
        };
        let out = tool.execute(&call("ut1", json!({ "task_id": "x" }))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn update_task_success() {
        let dir = TempDir::new().unwrap();
        let h = store_handle(&dir);
        let task_id = {
            let s = h.lock().await;
            s.create_task("T1", "original", "alice", vec![]).unwrap()
        };
        let tool = UpdateTaskTool { store: h.clone() };
        let out = tool
            .execute(&call(
                "ut1",
                json!({ "task_id": task_id, "description": "updated" }),
            ))
            .await;
        assert!(!out.is_error);
        let s = h.lock().await;
        let list = s.load().unwrap();
        assert_eq!(list.tasks[0].description, "updated");
    }
}
