// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::events::{TodoItem, TodoStatus, ToolEvent};
use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolDisplay, ToolOutput};

pub struct TodoTool {
    todos: Arc<Mutex<Vec<TodoItem>>>,
    event_tx: mpsc::Sender<ToolEvent>,
}

impl TodoTool {
    pub fn new(todos: Arc<Mutex<Vec<TodoItem>>>, event_tx: mpsc::Sender<ToolEvent>) -> Self {
        Self { todos, event_tx }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Read or manage the session todo list.\n\
         Actions:\n\
         - `read`   (default) — return the current list; no other fields needed.\n\
         - `add`    — append new items; `todos` array required (id, content, status).\n\
         - `update` — change status of existing items by id; `todos` array of {id, status} required.\n\
         - `set`    — replace the entire list; `todos` array required (empty list clears all).\n\
         At most one item may be `in_progress` at a time (enforced on add/set).\n\
         Mark items `completed` immediately after finishing. Update silently — never announce changes.\n\
         Use for: complex multi-step tasks (3+ steps). Skip for: trivial/single tasks.\n\
         statuses: pending | in_progress | completed | cancelled"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "add", "update", "set"],
                    "description": "Operation: read (default), add, update, or set."
                },
                "todos": {
                    "type": "array",
                    "description": "Items for add/update/set. For `update` only `id` and `status` are required.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id":      { "type": "string" },
                            "content": { "type": "string" },
                            "status":  {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"]
                            }
                        },
                        "required": ["id", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = call
            .args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");

        match action {
            "read" => {
                let current = self.todos.lock().await;
                ToolOutput::ok(&call.id, format_todos(&current))
            }

            "add" => {
                let new_items = match parse_full_items(call) {
                    Ok(v) => v,
                    Err(e) => return ToolOutput::err(&call.id, e),
                };
                let mut todos = self.todos.lock().await;
                todos.extend(new_items);
                if let Err(e) = check_in_progress(&todos) {
                    let added = call.args["todos"].as_array().map_or(0, |a| a.len());
                    let new_len = todos.len().saturating_sub(added);
                    todos.truncate(new_len);
                    return ToolOutput::err(&call.id, e);
                }
                debug!(count = todos.len(), "todo add");
                let snapshot = todos.clone();
                drop(todos);
                self.emit(&snapshot).await;
                ToolOutput::ok(&call.id, format_todos(&snapshot))
            }

            "update" => {
                let patches = match parse_patch_items(call) {
                    Ok(v) => v,
                    Err(e) => return ToolOutput::err(&call.id, e),
                };
                let mut todos = self.todos.lock().await;
                for (id, status, content) in &patches {
                    match todos.iter_mut().find(|t| &t.id == id) {
                        Some(item) => {
                            item.status = status.clone();
                            if let Some(c) = content {
                                item.content = c.clone();
                            }
                        }
                        None => {
                            return ToolOutput::err(
                                &call.id,
                                format!("todo item '{id}' not found"),
                            );
                        }
                    }
                }
                if let Err(e) = check_in_progress(&todos) {
                    return ToolOutput::err(&call.id, e);
                }
                debug!(count = todos.len(), "todo update");
                let snapshot = todos.clone();
                drop(todos);
                self.emit(&snapshot).await;
                ToolOutput::ok(&call.id, format_todos(&snapshot))
            }

            "set" => {
                let items = match call.args.get("todos").and_then(|v| v.as_array()) {
                    Some(arr) => {
                        let mut out = Vec::with_capacity(arr.len());
                        for (i, item) in arr.iter().enumerate() {
                            match serde_json::from_value::<TodoItem>(item.clone()) {
                                Ok(t) => out.push(t),
                                Err(e) => {
                                    let fallback = format!("item {}", i + 1);
                                    let label = item
                                        .get("id")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(&fallback);
                                    return ToolOutput::err(
                                        &call.id,
                                        format!("invalid todo '{label}': {e}"),
                                    );
                                }
                            }
                        }
                        out
                    }
                    None => {
                        return ToolOutput::err(&call.id, "`todos` array required for action=set")
                    }
                };
                if let Err(e) = check_in_progress(&items) {
                    return ToolOutput::err(&call.id, e);
                }
                debug!(count = items.len(), "todo set");
                *self.todos.lock().await = items.clone();
                self.emit(&items).await;
                ToolOutput::ok(&call.id, format_todos(&items))
            }

            other => ToolOutput::err(
                &call.id,
                format!("unknown action '{other}': must be read, add, update, or set"),
            ),
        }
    }
}

impl TodoTool {
    async fn emit(&self, items: &[TodoItem]) {
        let _ = self
            .event_tx
            .send(ToolEvent::TodoUpdate(items.to_vec()))
            .await;
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_full_items(call: &ToolCall) -> Result<Vec<TodoItem>, String> {
    let arr = call
        .args
        .get("todos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "`todos` array required".to_string())?;

    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        if item.get("content").and_then(|v| v.as_str()).is_none() {
            let fallback = format!("item {}", i + 1);
            let label = item.get("id").and_then(|v| v.as_str()).unwrap_or(&fallback);
            return Err(format!(
                "todo '{label}' is missing required field 'content'"
            ));
        }
        match serde_json::from_value::<TodoItem>(item.clone()) {
            Ok(t) => out.push(t),
            Err(e) => {
                let fallback = format!("item {}", i + 1);
                let label = item.get("id").and_then(|v| v.as_str()).unwrap_or(&fallback);
                return Err(format!("invalid todo '{label}': {e}"));
            }
        }
    }
    Ok(out)
}

/// Returns Vec<(id, status, Option<content>)> for `update` patches.
fn parse_patch_items(call: &ToolCall) -> Result<Vec<(String, TodoStatus, Option<String>)>, String> {
    let arr = call
        .args
        .get("todos")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "`todos` array required for action=update".to_string())?;

    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        let fallback = format!("item {}", i + 1);
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("todo {} missing 'id'", i + 1))?
            .to_string();
        let status_str = item
            .get("status")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("todo '{}' missing 'status'", &fallback))?;
        let status = serde_json::from_value::<TodoStatus>(Value::String(status_str.to_string()))
            .map_err(|_| {
                format!(
                    "todo '{id}' has invalid status '{status_str}': must be pending, in_progress, completed, or cancelled"
                )
            })?;
        let content = item
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        out.push((id, status, content));
    }
    Ok(out)
}

fn check_in_progress(items: &[TodoItem]) -> Result<(), String> {
    let count = items
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    if count > 1 {
        Err("at most one todo can be 'in_progress' at a time".into())
    } else {
        Ok(())
    }
}

fn format_todos(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Todo list is empty.".to_string();
    }
    items
        .iter()
        .map(|t| format!("{} [{}] {}", t.status.icon(), t.id, t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

impl ToolDisplay for TodoTool {
    fn display_name(&self) -> &str {
        "Todos"
    }
    fn icon(&self) -> &str {
        "☑"
    }
    fn category(&self) -> &str {
        "system"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("read");
        match action {
            "read" => "read".into(),
            "add" | "update" | "set" => {
                if let Some(todos) = args.get("todos").and_then(|v| v.as_array()) {
                    let n = todos.len();
                    format!("{action} {n} item{}", if n == 1 { "" } else { "s" })
                } else {
                    action.into()
                }
            }
            _ => action.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn make_tool() -> (
        TodoTool,
        Arc<Mutex<Vec<TodoItem>>>,
        mpsc::Receiver<ToolEvent>,
    ) {
        let todos = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel(16);
        (TodoTool::new(todos.clone(), tx), todos, rx)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "todo".into(),
            args,
        }
    }

    // ── read ──────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn read_empty() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool.execute(&call(json!({}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("empty"), "{}", out.content);
    }

    #[tokio::test]
    async fn read_existing() {
        let (tool, todos, _rx) = make_tool();
        todos.lock().await.push(TodoItem {
            id: "x".into(),
            content: "existing task".into(),
            status: TodoStatus::Pending,
        });
        let out = tool.execute(&call(json!({ "action": "read" }))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("existing task"), "{}", out.content);
    }

    // ── add ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn add_items() {
        let (tool, todos, _rx) = make_tool();
        let out = tool
            .execute(&call(json!({
                "action": "add",
                "todos": [
                    {"id": "1", "content": "first", "status": "in_progress"},
                    {"id": "2", "content": "second", "status": "pending"}
                ]
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(todos.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn add_requires_content() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool
            .execute(&call(json!({
                "action": "add",
                "todos": [{"id": "1", "status": "pending"}]
            })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("content"), "{}", out.content);
    }

    // ── update ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_single_status() {
        let (tool, todos, _rx) = make_tool();
        todos.lock().await.push(TodoItem {
            id: "task1".into(),
            content: "do something".into(),
            status: TodoStatus::Pending,
        });
        let out = tool
            .execute(&call(json!({
                "action": "update",
                "todos": [{"id": "task1", "status": "completed"}]
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(todos.lock().await[0].status, TodoStatus::Completed);
        // Content preserved
        assert_eq!(todos.lock().await[0].content, "do something");
    }

    #[tokio::test]
    async fn update_nonexistent_id_is_error() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool
            .execute(&call(json!({
                "action": "update",
                "todos": [{"id": "ghost", "status": "completed"}]
            })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("not found"), "{}", out.content);
    }

    // ── set ───────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn set_replaces_list() {
        let (tool, todos, _rx) = make_tool();
        todos.lock().await.push(TodoItem {
            id: "old".into(),
            content: "old task".into(),
            status: TodoStatus::Pending,
        });
        let out = tool
            .execute(&call(json!({
                "action": "set",
                "todos": [{"id": "new", "content": "new task", "status": "pending"}]
            })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        let locked = todos.lock().await;
        assert_eq!(locked.len(), 1);
        assert_eq!(locked[0].id, "new");
    }

    #[tokio::test]
    async fn set_empty_clears_list() {
        let (tool, todos, _rx) = make_tool();
        todos.lock().await.push(TodoItem {
            id: "x".into(),
            content: "task".into(),
            status: TodoStatus::Pending,
        });
        let out = tool
            .execute(&call(json!({ "action": "set", "todos": [] })))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(todos.lock().await.is_empty());
        assert!(out.content.contains("empty"), "{}", out.content);
    }

    // ── constraints ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn rejects_multiple_in_progress() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool
            .execute(&call(json!({
                "action": "set",
                "todos": [
                    {"id": "1", "content": "a", "status": "in_progress"},
                    {"id": "2", "content": "b", "status": "in_progress"}
                ]
            })))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("at most one"), "{}", out.content);
    }

    #[tokio::test]
    async fn emits_tool_event_on_set() {
        let (tool, _todos, mut rx) = make_tool();
        tool.execute(&call(json!({
            "action": "set",
            "todos": [{"id": "a", "content": "task", "status": "pending"}]
        })))
        .await;
        assert!(matches!(rx.try_recv(), Ok(ToolEvent::TodoUpdate(_))));
    }

    #[tokio::test]
    async fn emits_tool_event_on_update() {
        let (tool, todos, mut rx) = make_tool();
        todos.lock().await.push(TodoItem {
            id: "t".into(),
            content: "task".into(),
            status: TodoStatus::Pending,
        });
        tool.execute(&call(json!({
            "action": "update",
            "todos": [{"id": "t", "status": "completed"}]
        })))
        .await;
        assert!(matches!(rx.try_recv(), Ok(ToolEvent::TodoUpdate(_))));
    }
}
