// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compound `memory` tool that consolidates persistent KV memory and project
//! knowledge into a single action-dispatched interface.
//!
//! Replaces three separate tools (`update_memory`, `list_knowledge`,
//! `search_knowledge`) with one unified tool, reducing the model's
//! tool-selection surface area.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_runtime::SharedKnowledge;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::update_memory::UpdateMemoryTool;
use crate::builtin::{
    knowledge::list_knowledge::ListKnowledgeTool, search::search_knowledge::SearchKnowledgeTool,
};

/// Compound memory tool — persistent KV store and project knowledge in one.
pub struct MemoryTool {
    memory: UpdateMemoryTool,
    list_knowledge: ListKnowledgeTool,
    search_knowledge: SearchKnowledgeTool,
}

impl MemoryTool {
    pub fn new(memory_file: Option<String>, knowledge: SharedKnowledge) -> Self {
        Self {
            memory: UpdateMemoryTool { memory_file },
            list_knowledge: ListKnowledgeTool {
                knowledge: knowledge.clone(),
            },
            search_knowledge: SearchKnowledgeTool { knowledge },
        }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Persistent memory and project knowledge access.\n\
         action: set | get | delete | list | search_knowledge | list_knowledge\n\n\
         KV memory (set/get/delete/list) persists across sessions in ~/.config/sven/memory.json.\n\
         At session start: call action=list to check stored project context.\n\
         Store: project conventions, toolchain quirks, recurring solutions.\n\n\
         Knowledge (search_knowledge/list_knowledge) searches .sven/knowledge/ docs.\n\
         Use search_knowledge before modifying a subsystem to retrieve architecture notes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["set", "get", "delete", "list", "search_knowledge", "list_knowledge"],
                    "description": "Which memory operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "[action=set|get|delete] Memory key"
                },
                "value": {
                    "type": "string",
                    "description": "[action=set] Value to store"
                },
                "query": {
                    "type": "string",
                    "description": "[action=search_knowledge] Keyword or phrase to search for"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        match action.as_str() {
            "set" | "get" | "delete" | "list" => {
                // Remap 'action' → 'operation' for the UpdateMemoryTool delegate.
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    let action_val = obj.remove("action").unwrap_or(json!("list"));
                    obj.insert("operation".to_string(), action_val);
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "update_memory".into(),
                    args,
                };
                self.memory.execute(&delegate_call).await
            }
            "list_knowledge" => {
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "list_knowledge".into(),
                    args: json!({}),
                };
                self.list_knowledge.execute(&delegate_call).await
            }
            "search_knowledge" => {
                let query = match call.args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'query' for action=search_knowledge",
                        )
                    }
                };
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "search_knowledge".into(),
                    args: json!({ "query": query }),
                };
                self.search_knowledge.execute(&delegate_call).await
            }
            other => ToolOutput::err(
                &call.id,
                format!(
                    "unknown action '{}'. Valid: set, get, delete, list, search_knowledge, list_knowledge",
                    other
                ),
            ),
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_runtime::SharedKnowledge;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn make_tool() -> MemoryTool {
        use std::sync::atomic::{AtomicU32, Ordering};
        static CTR: AtomicU32 = AtomicU32::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        MemoryTool::new(
            Some(format!(
                "/tmp/sven_memory_compound_{}_{n}.json",
                std::process::id()
            )),
            SharedKnowledge::empty(),
        )
    }

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "m1".into(),
            name: "memory".into(),
            args,
        }
    }

    #[test]
    fn name_is_memory() {
        let t = make_tool();
        assert_eq!(t.name(), "memory");
    }

    #[tokio::test]
    async fn missing_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'action'"));
    }

    #[tokio::test]
    async fn unknown_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "fly"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn set_and_get_round_trip() {
        let t = make_tool();
        let path = t.memory.memory_file.clone().unwrap();

        t.execute(&call(
            json!({"action": "set", "key": "proj", "value": "sven"}),
        ))
        .await;
        let out = t
            .execute(&call(json!({"action": "get", "key": "proj"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "sven");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn list_returns_keys() {
        let t = make_tool();
        let path = t.memory.memory_file.clone().unwrap();

        t.execute(&call(json!({"action": "set", "key": "a", "value": "1"})))
            .await;
        let out = t.execute(&call(json!({"action": "list"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("a"));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn list_knowledge_with_empty_store() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "list_knowledge"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No knowledge documents found"));
    }

    #[tokio::test]
    async fn search_knowledge_missing_query_is_error() {
        let t = make_tool();
        let out = t
            .execute(&call(json!({"action": "search_knowledge"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("query"));
    }

    #[tokio::test]
    async fn search_knowledge_with_empty_store() {
        let t = make_tool();
        let out = t
            .execute(&call(
                json!({"action": "search_knowledge", "query": "relay"}),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("No knowledge documents found"));
    }
}
