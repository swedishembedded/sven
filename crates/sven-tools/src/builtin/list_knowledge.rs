// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
//! `list_knowledge` — enumerate all knowledge documents in `.sven/knowledge/`.

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_config::AgentMode;
use sven_runtime::SharedKnowledge;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

/// List all project knowledge documents with their subsystem names, covered
/// file patterns, and last-updated dates.
///
/// Call this first to discover which subsystems have knowledge specs, then
/// use `search_knowledge` to find relevant content or `read_file` to load
/// an entire document.
pub struct ListKnowledgeTool {
    pub knowledge: SharedKnowledge,
}

#[async_trait]
impl Tool for ListKnowledgeTool {
    fn name(&self) -> &str {
        "list_knowledge"
    }

    fn description(&self) -> &str {
        "List all project knowledge documents in `.sven/knowledge/`.\n\
         Returns: subsystem name, covered file patterns, last-updated date, filename.\n\
         Use `search_knowledge` to search content, or `read_file` to load a full doc.\n\
         Knowledge docs contain subsystem architecture, invariants, and failure-mode tables."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Research, AgentMode::Plan, AgentMode::Agent]
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::MatchList
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let docs = self.knowledge.get();

        if docs.is_empty() {
            return ToolOutput::ok(
                &call.id,
                "No knowledge documents found.\n\
                 Create `.sven/knowledge/<subsystem>.md` files with YAML frontmatter:\n\
                 ```\n\
                 ---\n\
                 subsystem: My Subsystem\n\
                 files:\n\
                   - crates/my-crate/**\n\
                 updated: 2026-01-01\n\
                 ---\n\
                 ```",
            );
        }

        let mut lines = vec![
            format!("Found {} knowledge document(s):\n", docs.len()),
            format!(
                "{:<30} {:<40} {:<12} {}",
                "Subsystem", "Covers", "Updated", "File"
            ),
            format!("{}", "-".repeat(100)),
        ];

        for doc in docs.iter() {
            let covers = if doc.files.is_empty() {
                "(no file patterns)".to_string()
            } else {
                doc.files.join(", ")
            };
            let covers_display = if covers.len() > 38 {
                format!("{}…", &covers[..37])
            } else {
                covers
            };

            let updated = doc.updated.as_deref().unwrap_or("—");
            let filename = doc.path.file_name().and_then(|n| n.to_str()).unwrap_or("?");

            lines.push(format!(
                "{:<30} {:<40} {:<12} {}",
                &doc.subsystem[..doc.subsystem.len().min(29)],
                covers_display,
                updated,
                filename
            ));
        }

        lines.push(String::new());
        lines.push(
            "Use `search_knowledge \"<query>\"` to find relevant content across all docs."
                .to_string(),
        );

        ToolOutput::ok(&call.id, lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{Tool, ToolCall};
    use serde_json::json;
    use sven_runtime::KnowledgeInfo;

    fn call() -> ToolCall {
        ToolCall {
            id: "lk1".into(),
            name: "list_knowledge".into(),
            args: json!({}),
        }
    }

    #[tokio::test]
    async fn empty_knowledge_base_returns_helpful_message() {
        let t = ListKnowledgeTool {
            knowledge: SharedKnowledge::empty(),
        };
        let out = t.execute(&call()).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No knowledge documents found"));
        assert!(out.content.contains(".sven/knowledge/"));
    }

    #[tokio::test]
    async fn lists_documents_with_metadata() {
        let docs = vec![
            KnowledgeInfo {
                subsystem: "P2P Networking".to_string(),
                files: vec!["crates/sven-p2p/**".to_string()],
                updated: Some("2026-01-15".to_string()),
                path: std::path::PathBuf::from(".sven/knowledge/sven-p2p.md"),
                body: "P2P body.".to_string(),
            },
            KnowledgeInfo {
                subsystem: "Tool System".to_string(),
                files: vec!["crates/sven-tools/**".to_string()],
                updated: None,
                path: std::path::PathBuf::from(".sven/knowledge/sven-tools.md"),
                body: "Tools body.".to_string(),
            },
        ];

        let t = ListKnowledgeTool {
            knowledge: SharedKnowledge::new(docs),
        };
        let out = t.execute(&call()).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("P2P Networking"));
        assert!(out.content.contains("Tool System"));
        assert!(out.content.contains("2026-01-15"));
        assert!(out.content.contains("sven-p2p.md"));
        assert!(out.content.contains("search_knowledge"));
    }
}
