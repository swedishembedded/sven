// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Agent tools for the shared team task list.
//!
//! These tools are registered in the node agent when it is part of a team.
//! The LLM can use them to create, claim, complete, and list tasks.
//!
//! All six tools open the [`TaskStore`] lazily in `execute()` via the team
//! name stored in [`TeamConfigHandle`].  No pre-opened handle is required at
//! registration time, so the tools are always safe to register unconditionally
//! whenever a [`TeamConfigHandle`] is available.

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{ApprovalPolicy, Tool, ToolCall, ToolOutput};

use crate::spawn::TeamConfigHandle;
use crate::task::{Task, TaskStatus, TaskStore};

// ── CreateTaskTool ───────────────────────────────────────────────────────────

/// Create a new task in the team's shared task list.
pub struct CreateTaskTool {
    pub team_config: TeamConfigHandle,
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

        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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
    pub team_config: TeamConfigHandle,
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
        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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
    pub team_config: TeamConfigHandle,
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

        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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
    pub team_config: TeamConfigHandle,
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

        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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
    pub team_config: TeamConfigHandle,
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

        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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
    pub team_config: TeamConfigHandle,
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

        let team_name = {
            let guard = self.team_config.lock().await;
            match guard.as_ref() {
                Some(c) => c.name.clone(),
                None => return ToolOutput::err(&call.id, "No active team. Use create_team first."),
            }
        };
        let store = match TaskStore::open(&team_name) {
            Ok(s) => s,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to open task store: {e}")),
        };

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

    use crate::config::TeamConfig;
    use crate::spawn::TeamConfigHandle;
    use crate::task::TaskStore;

    use super::*;

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: String::new(),
            args,
        }
    }

    /// Build a `TeamConfigHandle` pre-populated with a team whose task store
    /// lives under `dir`.  The `TaskStore` is opened to initialise the file.
    fn config_handle(dir: &TempDir, team_name: &str) -> TeamConfigHandle {
        // Create the task store file so TaskStore::open succeeds later.
        let tasks_path = dir.path().join("tasks.json");
        TaskStore::open_at(tasks_path).expect("create task store");

        // Point the global team dir at our temp dir by creating the expected
        // path structure.  The real TaskStore::open resolves to
        // ~/.config/sven/teams/{name}/tasks.json, which we can't easily
        // redirect in tests.  Use TaskStore::open_at in tests that need the
        // store directly; here we just need a config handle for tools that
        // will call TaskStore::open(&team_name) internally.
        //
        // For tools tests we therefore use a unique team name that doesn't
        // collide in the real directory and accept that the store is created
        // in the real location during the test run.  The test data is minimal
        // and scoped to the test's unique name.
        let cfg = TeamConfig::new(team_name, "peer-lead", "alice");
        Arc::new(Mutex::new(Some(cfg)))
    }

    fn empty_config() -> TeamConfigHandle {
        Arc::new(Mutex::new(None))
    }

    // ── Tool metadata (smoke tests) ───────────────────────────────────────────

    #[test]
    fn tool_names_are_stable() {
        let h = empty_config();
        assert_eq!(
            CreateTaskTool {
                team_config: h.clone(),
                agent_name: "a".into()
            }
            .name(),
            "create_task"
        );
        assert_eq!(
            ClaimTaskTool {
                team_config: h.clone(),
                agent_name: "a".into()
            }
            .name(),
            "claim_task"
        );
        assert_eq!(
            CompleteTaskTool {
                team_config: h.clone()
            }
            .name(),
            "complete_task"
        );
        assert_eq!(
            ListTasksTool {
                team_config: h.clone()
            }
            .name(),
            "list_tasks"
        );
        assert_eq!(
            AssignTaskTool {
                team_config: h.clone()
            }
            .name(),
            "assign_task"
        );
        assert_eq!(
            UpdateTaskTool {
                team_config: h.clone()
            }
            .name(),
            "update_task"
        );
    }

    #[test]
    fn all_tools_have_auto_policy() {
        let h = empty_config();
        use sven_tools::ApprovalPolicy;
        assert_eq!(
            CreateTaskTool {
                team_config: h.clone(),
                agent_name: "a".into()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            ClaimTaskTool {
                team_config: h.clone(),
                agent_name: "a".into()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            CompleteTaskTool {
                team_config: h.clone()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            ListTasksTool {
                team_config: h.clone()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            AssignTaskTool {
                team_config: h.clone()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
        assert_eq!(
            UpdateTaskTool {
                team_config: h.clone()
            }
            .default_policy(),
            ApprovalPolicy::Auto
        );
    }

    // ── Error cases when no active team ──────────────────────────────────────

    #[tokio::test]
    async fn create_task_no_team_is_error() {
        let tool = CreateTaskTool {
            team_config: empty_config(),
            agent_name: "alice".into(),
        };
        let out = tool
            .execute(&call("c1", json!({ "title": "T", "description": "d" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn claim_task_no_team_is_error() {
        let tool = ClaimTaskTool {
            team_config: empty_config(),
            agent_name: "alice".into(),
        };
        let out = tool.execute(&call("c1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn complete_task_no_team_is_error() {
        let tool = CompleteTaskTool {
            team_config: empty_config(),
        };
        let out = tool
            .execute(&call("c1", json!({ "task_id": "x", "summary": "done" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn list_tasks_no_team_is_error() {
        let tool = ListTasksTool {
            team_config: empty_config(),
        };
        let out = tool.execute(&call("l1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn assign_task_no_team_is_error() {
        let tool = AssignTaskTool {
            team_config: empty_config(),
        };
        let out = tool
            .execute(&call("a1", json!({ "task_id": "x", "assignee": "bob" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    #[tokio::test]
    async fn update_task_no_team_is_error() {
        let tool = UpdateTaskTool {
            team_config: empty_config(),
        };
        let out = tool
            .execute(&call("u1", json!({ "task_id": "x", "description": "y" })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active team"));
    }

    // ── Parameter validation ──────────────────────────────────────────────────

    #[test]
    fn create_task_missing_title_returns_error_on_meta() {
        // Verify the schema requires title.
        let h = empty_config();
        let tool = CreateTaskTool {
            team_config: h,
            agent_name: "alice".into(),
        };
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("title")));
        assert!(required.iter().any(|v| v.as_str() == Some("description")));
    }

    // ── Integration: task lifecycle using real TaskStore path ─────────────────

    #[tokio::test]
    async fn full_task_lifecycle() {
        // Use a unique team name to avoid colliding with other tests or real teams.
        let team_name = format!("test-tools-{}", uuid::Uuid::new_v4().simple());
        let cfg = TeamConfig::new(&team_name, "peer-lead", "alice");
        let handle: TeamConfigHandle = Arc::new(Mutex::new(Some(cfg)));

        // Create the task store on disk so subsequent opens succeed.
        let store = TaskStore::open(&team_name).expect("open store");
        drop(store);

        let create_tool = CreateTaskTool {
            team_config: handle.clone(),
            agent_name: "alice".into(),
        };

        let out = create_tool
            .execute(&call(
                "c1",
                json!({ "title": "Do X", "description": "Details" }),
            ))
            .await;
        assert!(!out.is_error, "create failed: {}", out.content);
        assert!(out.content.contains("Do X"));

        // Extract task ID from output.
        let task_id = out
            .content
            .lines()
            .find(|l| l.starts_with("Task ID:"))
            .and_then(|l| l.strip_prefix("Task ID: "))
            .unwrap_or("")
            .trim()
            .to_string();
        assert!(!task_id.is_empty(), "no task ID in output");

        let claim_tool = ClaimTaskTool {
            team_config: handle.clone(),
            agent_name: "bob".into(),
        };
        let out = claim_tool
            .execute(&call("cl1", json!({ "task_id": &task_id })))
            .await;
        assert!(!out.is_error, "claim failed: {}", out.content);

        let complete_tool = CompleteTaskTool {
            team_config: handle.clone(),
        };
        let out = complete_tool
            .execute(&call(
                "ct1",
                json!({ "task_id": &task_id, "summary": "All done!" }),
            ))
            .await;
        assert!(!out.is_error, "complete failed: {}", out.content);

        let list_tool = ListTasksTool {
            team_config: handle.clone(),
        };
        let out = list_tool
            .execute(&call("lt1", json!({ "status_filter": "completed" })))
            .await;
        assert!(!out.is_error, "list failed: {}", out.content);
        assert!(out.content.contains("Do X"));

        // Clean up the team directory.
        let _ = std::fs::remove_dir_all(crate::task::default_team_dir(&team_name));
    }
}
