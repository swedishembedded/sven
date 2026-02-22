// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use crate::events::{TodoItem, ToolEvent};
use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct TodoWriteTool {
    todos: Arc<Mutex<Vec<TodoItem>>>,
    event_tx: mpsc::Sender<ToolEvent>,
}

impl TodoWriteTool {
    pub fn new(todos: Arc<Mutex<Vec<TodoItem>>>, event_tx: mpsc::Sender<ToolEvent>) -> Self {
        Self { todos, event_tx }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str { "todo_write" }

    fn description(&self) -> &str {
        "Create and manage a structured task list for the current session.\n\n\
         Note: Other than when first creating todos, don't tell the user you're updating \
         todos — just do it silently.\n\n\
         ## Task Statuses\n\
         - pending: Not yet started\n\
         - in_progress: Currently being worked on (only ONE at a time)\n\
         - completed: Finished successfully\n\
         - cancelled: No longer relevant\n\n\
         ## When to Use\n\
         Use proactively for:\n\
         - Complex multi-step tasks (3+ distinct steps)\n\
         - Non-trivial tasks requiring careful planning\n\
         - User provides multiple tasks to accomplish\n\
         - Long-running implementations with clear sub-steps\n\n\
         ## When NOT to Use\n\
         Skip for:\n\
         - Single, straightforward tasks\n\
         - Trivial tasks completable in fewer than 3 steps\n\
         - Purely conversational or informational requests\n\
         - Don't add a task to test the change unless asked\n\n\
         ## Parallel Todo Writes\n\
         - Prefer creating the first todo as in_progress\n\
         - Start working on todos by using other tool calls in the same turn as todo_write\n\
         - Batch todo status updates with other tool calls to reduce round-trips\n\n\
         ## Examples\n\
         <example>\n\
         Create initial task list and immediately start the first item:\n\
         todo_write: todos=[\n\
           {id=\"1\", content=\"Analyze codebase\", status=\"in_progress\"},\n\
           {id=\"2\", content=\"Design solution\", status=\"pending\"},\n\
           {id=\"3\", content=\"Implement changes\", status=\"pending\"}\n\
         ]\n\
         [Then call read_file or glob in the same turn to begin task 1]\n\
         </example>\n\
         <example>\n\
         Update after completing task:\n\
         todo_write: todos=[\n\
           {id=\"1\", content=\"Analyze codebase\", status=\"completed\"},\n\
           {id=\"2\", content=\"Design solution\", status=\"in_progress\"},\n\
           {id=\"3\", content=\"Implement changes\", status=\"pending\"}\n\
         ]\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Each item requires a unique id, content, and status\n\
         - Only one item in_progress at a time — enforced at execution\n\
         - Mark complete IMMEDIATELY after finishing each task\n\
         - Calling todo_write replaces the entire list (not a merge/patch)\n\
         - Complete current tasks before starting new ones"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "Array of todo items to set (replaces existing list)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Unique identifier for the task"
                            },
                            "content": {
                                "type": "string",
                                "description": "Description of the task"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Current status of the task"
                            }
                        },
                        "required": ["id", "content", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["todos"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let todos_value = match call.args.get("todos").and_then(|v| v.as_array()) {
            Some(arr) => arr.clone(),
            None => return ToolOutput::err(&call.id, "missing 'todos' array"),
        };

        let mut items: Vec<TodoItem> = Vec::new();
        for item in &todos_value {
            let id = match item.get("id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return ToolOutput::err(&call.id, "todo item missing 'id'"),
            };
            let content = match item.get("content").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return ToolOutput::err(&call.id, format!("todo '{id}' missing 'content'")),
            };
            let status = match item.get("status").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => return ToolOutput::err(&call.id, format!("todo '{id}' missing 'status'")),
            };
            if !["pending", "in_progress", "completed", "cancelled"].contains(&status.as_str()) {
                return ToolOutput::err(
                    &call.id,
                    format!("invalid status '{status}' for todo '{id}'"),
                );
            }
            items.push(TodoItem { id, content, status });
        }

        // Validate at most one in_progress
        let in_progress_count = items.iter().filter(|t| t.status == "in_progress").count();
        if in_progress_count > 1 {
            return ToolOutput::err(&call.id, "at most one todo can be 'in_progress' at a time");
        }

        debug!(count = items.len(), "todo_write tool");

        *self.todos.lock().await = items.clone();
        let _ = self.event_tx.send(ToolEvent::TodoUpdate(items.clone())).await;

        let summary = format_todos(&items);
        ToolOutput::ok(&call.id, summary)
    }
}

fn format_todos(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Todo list cleared.".to_string();
    }
    let lines: Vec<String> = items.iter().map(|t| {
        let icon = match t.status.as_str() {
            "completed" => "✓",
            "in_progress" => "→",
            "cancelled" => "✗",
            _ => "○",
        };
        format!("{icon} [{}] {}", t.id, t.content)
    }).collect();
    format!("Todos updated:\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn make_tool() -> (TodoWriteTool, Arc<Mutex<Vec<TodoItem>>>, mpsc::Receiver<ToolEvent>) {
        let todos = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = mpsc::channel(16);
        let tool = TodoWriteTool::new(todos.clone(), tx);
        (tool, todos, rx)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "todo_write".into(), args }
    }

    #[tokio::test]
    async fn sets_todos() {
        let (tool, todos, _rx) = make_tool();
        let out = tool.execute(&call(json!({
            "todos": [
                {"id": "1", "content": "do something", "status": "pending"},
                {"id": "2", "content": "in progress", "status": "in_progress"}
            ]
        }))).await;
        assert!(!out.is_error, "{}", out.content);
        let locked = todos.lock().await;
        assert_eq!(locked.len(), 2);
        assert_eq!(locked[0].id, "1");
    }

    #[tokio::test]
    async fn emits_tool_event() {
        let (tool, _todos, mut rx) = make_tool();
        tool.execute(&call(json!({
            "todos": [{"id": "a", "content": "task", "status": "pending"}]
        }))).await;
        let event = rx.try_recv().expect("should have emitted event");
        matches!(event, ToolEvent::TodoUpdate(_));
    }

    #[tokio::test]
    async fn rejects_multiple_in_progress() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool.execute(&call(json!({
            "todos": [
                {"id": "1", "content": "a", "status": "in_progress"},
                {"id": "2", "content": "b", "status": "in_progress"}
            ]
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("at most one"));
    }

    #[tokio::test]
    async fn missing_todos_is_error() {
        let (tool, _todos, _rx) = make_tool();
        let out = tool.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'todos'"));
    }
}
