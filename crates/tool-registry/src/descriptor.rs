// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in deferred tool
// disclosure for agents then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The compact description of a tool: what the model reads about a tool whose
//! full definition it has not loaded.
//!
//! A descriptor is derived from the tool itself, never declared beside it, so
//! it cannot drift: the summary is [`Tool::summary`], the risk class is the
//! tool's [`ToolCapability`] (the same bucket the kernel gates and audits it
//! under), and the category is that bucket's label. There is no taxonomy of
//! tools apart from the capabilities.

use serde_json::Value;
use sven_tool_api::{one_line_summary, Tool, ToolCapability, SUMMARY_MAX_CHARS};

/// The category of every tool that comes from an MCP server, whatever
/// capability it is gated under.
pub const MCP_CATEGORY: &str = "mcp";

/// The most parameters of one tool that are indexed for search.
const MAX_INDEXED_PARAMETERS: usize = 64;
/// The most characters of a description, or of a parameter's, that are indexed.
const MAX_INDEXED_CHARS: usize = 2_000;

/// A tool in one line: enough to decide whether to load it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolDescriptor {
    /// The name the model calls the tool by.
    pub name: String,
    /// What the tool is for, at most [`SUMMARY_MAX_CHARS`] characters.
    pub summary: String,
    /// The capability the kernel gates a call of this tool under (the widest
    /// effect any of its calls has).
    pub capability: ToolCapability,
    /// Whether the tool comes from an MCP server.
    pub mcp: bool,
    /// The whole description. Only search reads it: a word the summary leaves
    /// out is often in the sentences after it.
    pub description: String,
    /// What the tool's schema says each top-level parameter is for, in
    /// parameter-name order. Only search reads it.
    pub parameter_descriptions: Vec<String>,
}

impl ToolDescriptor {
    /// Describes `tool`. A tool that states no summary is summarised by the
    /// first sentence of its description.
    #[must_use]
    pub fn of(tool: &dyn Tool) -> Self {
        let stated = tool.summary().trim();
        let summary = if stated.is_empty() {
            one_line_summary(tool.description(), SUMMARY_MAX_CHARS)
        } else {
            stated.to_string()
        };
        Self {
            name: tool.name().to_string(),
            summary,
            capability: tool.kernel_capability(),
            mcp: tool.is_mcp(),
            description: truncated(tool.description()),
            parameter_descriptions: parameter_descriptions(&tool.parameters_schema()),
        }
    }

    /// What the tool is classed as when browsing: [`MCP_CATEGORY`] for an MCP
    /// tool, else the label of its capability.
    #[must_use]
    pub fn category(&self) -> &'static str {
        if self.mcp {
            MCP_CATEGORY
        } else {
            capability_label(self.capability)
        }
    }

    /// `true` when no call of the tool changes anything outside the run.
    #[must_use]
    pub fn read_only(&self) -> bool {
        self.capability.is_read_only()
    }

    /// The tool's line in an index: `name: summary`.
    #[must_use]
    pub fn index_line(&self) -> String {
        format!("{}: {}", self.name, self.summary)
    }
}

fn truncated(text: &str) -> String {
    text.chars().take(MAX_INDEXED_CHARS).collect()
}

/// The descriptions of the top-level properties of a JSON Schema object, in
/// property-name order.
fn parameter_descriptions(schema: &Value) -> Vec<String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut named: Vec<(&String, String)> = properties
        .iter()
        .take(MAX_INDEXED_PARAMETERS)
        .filter_map(|(name, property)| {
            let text = property.get("description")?.as_str()?;
            Some((name, truncated(text)))
        })
        .collect();
    named.sort();
    named.into_iter().map(|(_, text)| text).collect()
}

/// A short lowercase label for a capability, the vocabulary a model filters
/// tools by.
#[must_use]
pub fn capability_label(capability: ToolCapability) -> &'static str {
    match capability {
        ToolCapability::ReadFile => "read",
        ToolCapability::WriteFile => "write",
        ToolCapability::DeleteFile => "delete",
        ToolCapability::ExecuteShell => "shell",
        ToolCapability::NetworkAccess => "network",
        ToolCapability::GitOperation => "git",
        ToolCapability::RunVerifier => "verify",
        ToolCapability::ControlDevice => "device",
        ToolCapability::SpawnChild => "agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_descriptions_come_from_the_schema_in_name_order() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "zone": {"type": "string", "description": "Where."},
                "format": {"type": "string", "enum": ["csv"], "description": "File format."},
                "plain": {"type": "integer"}
            }
        });
        assert_eq!(parameter_descriptions(&schema), ["File format.", "Where."]);
        assert!(parameter_descriptions(&serde_json::json!({"type": "object"})).is_empty());
    }

    #[test]
    fn every_capability_has_a_distinct_label() {
        let mut labels: Vec<_> = ToolCapability::ALL
            .iter()
            .map(|c| capability_label(*c))
            .collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), ToolCapability::ALL.len());
    }
}
