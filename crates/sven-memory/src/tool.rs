// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `memory` extended tool with semantic remember/recall/forget.
//!
//! This tool extends the basic KV memory with full-text and (optionally)
//! vector-similarity search, making it suitable for a "second brain" use case.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use serde_json::{json, Value};

use sven_tools::{
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolDisplay, ToolOutput},
};

use crate::{
    store::{Document, VectorStore},
    DocId,
};

/// Semantic memory tool backed by a [`VectorStore`].
///
/// # Actions
///
/// - `remember` — store a fact, note, or contact detail
/// - `recall` — semantic search for relevant memories
/// - `forget` — delete a memory by ID
/// - `list` — list all stored memories
/// - `get` — retrieve a specific memory by ID
///
/// # Use cases
///
/// - **Second Brain**: Text anything to remember → `remember`; search later → `recall`
/// - **CRM**: Save contact notes → `remember { entity: "Alice" }`; query → `recall "Alice"`
/// - **Action items**: Save meeting notes → recall before calls
pub struct SemanticMemoryTool {
    store: Arc<dyn VectorStore>,
}

impl SemanticMemoryTool {
    pub fn new(store: Arc<dyn VectorStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for SemanticMemoryTool {
    fn name(&self) -> &str {
        "semantic_memory"
    }

    fn description(&self) -> &str {
        "Persistent semantic memory: store and recall facts, notes, contacts, and action items.\n\
         Actions: remember | recall | forget | list | get\n\
         Use remember to save any information worth keeping. Use recall with a natural language \
         query to find relevant stored memories. Perfect for second-brain knowledge management, \
         personal CRM, and tracking action items across sessions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "forget", "list", "get"],
                    "description": "Memory operation to perform"
                },
                "content": {
                    "type": "string",
                    "description": "(remember) Text content to store"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "(remember) Tags for categorization (e.g. ['contact', 'alice', 'acme'])"
                },
                "entity": {
                    "type": "string",
                    "description": "(remember) Person, company, or topic this memory is about"
                },
                "source": {
                    "type": "string",
                    "description": "(remember) Source of this memory (e.g. 'email', 'calendar', 'user')"
                },
                "query": {
                    "type": "string",
                    "description": "(recall) Natural language query to search memories"
                },
                "limit": {
                    "type": "integer",
                    "description": "(recall/list) Maximum number of results. Default: 10"
                },
                "tag_filter": {
                    "type": "string",
                    "description": "(list) Filter memories by this tag/entity substring"
                },
                "id": {
                    "type": "integer",
                    "description": "(forget/get) Memory ID"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolOutput::err(&call.id, "missing 'action'"),
        };

        match action {
            "remember" => self.remember(call).await,
            "recall" => self.recall(call).await,
            "forget" => self.forget(call).await,
            "list" => self.list(call).await,
            "get" => self.get(call).await,
            other => ToolOutput::err(
                &call.id,
                format!("unknown action {other:?}; expected remember|recall|forget|list|get"),
            ),
        }
    }
}

impl SemanticMemoryTool {
    async fn remember(&self, call: &ToolCall) -> ToolOutput {
        let content = match call.args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err(&call.id, "remember requires 'content'"),
        };

        let mut metadata = HashMap::new();

        if let Some(entity) = call.args.get("entity").and_then(|v| v.as_str()) {
            metadata.insert("entity".to_string(), entity.to_string());
        }
        if let Some(source) = call.args.get("source").and_then(|v| v.as_str()) {
            metadata.insert("source".to_string(), source.to_string());
        }
        if let Some(tags) = call.args.get("tags").and_then(|v| v.as_array()) {
            let tag_str = tags
                .iter()
                .filter_map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(",");
            metadata.insert("tags".to_string(), tag_str);
        }
        metadata.insert(
            "created".to_string(),
            chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string(),
        );

        let doc = Document {
            content: content.clone(),
            metadata,
            embedding: None,
        };

        match self.store.insert(doc).await {
            Ok(id) => ToolOutput::ok(
                &call.id,
                format!("Remembered (ID={id}): {}", truncate(&content, 80)),
            ),
            Err(e) => ToolOutput::err(&call.id, format!("remember failed: {e}")),
        }
    }

    async fn recall(&self, call: &ToolCall) -> ToolOutput {
        let query = match call.args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolOutput::err(&call.id, "recall requires 'query'"),
        };
        let limit = call
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        match self.store.search(&query, limit).await {
            Ok(results) => {
                if results.is_empty() {
                    return ToolOutput::ok(&call.id, "No memories found for that query.");
                }
                let mut lines = vec![format!("Found {} memories:", results.len())];
                for r in &results {
                    let entity = r
                        .metadata
                        .get("entity")
                        .map(|s| format!(" [{s}]"))
                        .unwrap_or_default();
                    let source = r
                        .metadata
                        .get("source")
                        .map(|s| format!(" (via {s})"))
                        .unwrap_or_default();
                    lines.push(format!(
                        "- ID={}{entity}{source}: {}",
                        r.id,
                        truncate(&r.content, 120)
                    ));
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Err(e) => ToolOutput::err(&call.id, format!("recall failed: {e}")),
        }
    }

    async fn forget(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_i64()) {
            Some(id) => id as DocId,
            None => return ToolOutput::err(&call.id, "forget requires 'id'"),
        };

        match self.store.delete(id).await {
            Ok(true) => ToolOutput::ok(&call.id, format!("Memory {id} deleted.")),
            Ok(false) => ToolOutput::err(&call.id, format!("No memory with ID {id}.")),
            Err(e) => ToolOutput::err(&call.id, format!("forget failed: {e}")),
        }
    }

    async fn list(&self, call: &ToolCall) -> ToolOutput {
        let tag_filter = call.args.get("tag_filter").and_then(|v| v.as_str());

        match self.store.list(tag_filter).await {
            Ok(summaries) => {
                if summaries.is_empty() {
                    return ToolOutput::ok(&call.id, "No memories stored.");
                }
                let limit = call
                    .args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20) as usize;
                let mut lines = vec![format!("{} memories stored:", summaries.len())];
                for s in summaries.iter().take(limit) {
                    let entity = s
                        .metadata
                        .get("entity")
                        .map(|e| format!(" [{e}]"))
                        .unwrap_or_default();
                    lines.push(format!("- ID={}{entity}: {}", s.id, s.snippet));
                }
                ToolOutput::ok(&call.id, lines.join("\n"))
            }
            Err(e) => ToolOutput::err(&call.id, format!("list failed: {e}")),
        }
    }

    async fn get(&self, call: &ToolCall) -> ToolOutput {
        let id = match call.args.get("id").and_then(|v| v.as_i64()) {
            Some(id) => id as DocId,
            None => return ToolOutput::err(&call.id, "get requires 'id'"),
        };

        match self.store.get(id).await {
            Ok(Some(doc)) => {
                let meta_display: String = doc
                    .metadata
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                ToolOutput::ok(
                    &call.id,
                    format!("Memory {id}:\n{}\n\n[{meta_display}]", doc.content),
                )
            }
            Ok(None) => ToolOutput::err(&call.id, format!("No memory with ID {id}.")),
            Err(e) => ToolOutput::err(&call.id, format!("get failed: {e}")),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

impl ToolDisplay for SemanticMemoryTool {
    fn display_name(&self) -> &str {
        "SemanticMemory"
    }
    fn icon(&self) -> &str {
        "🧠"
    }
    fn category(&self) -> &str {
        "memory"
    }
    fn collapsed_summary(&self, args: &Value) -> String {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        match action {
            "remember" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                format!("remember: {}", truncate(content, 40))
            }
            "recall" => {
                let q = args.get("query").and_then(|v| v.as_str()).unwrap_or("?");
                format!("recall '{q}'")
            }
            _ => action.to_string(),
        }
    }
}
