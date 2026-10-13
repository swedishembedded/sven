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

use sven_tool_api::{one_line_summary, Tool, ToolCapability, SUMMARY_MAX_CHARS};

/// The category of every tool that comes from an MCP server, whatever
/// capability it is gated under.
pub const MCP_CATEGORY: &str = "mcp";

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
