// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between MCP tool definitions and sven's `Tool` trait.
//!
//! `McpTool` wraps an MCP tool definition and routes execution through the
//! `McpManager`.  It integrates transparently with `sven_tools::ToolRegistry`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use sven_tools::{ApprovalPolicy, OutputCategory, Tool, ToolCall, ToolOutput};

use crate::manager::McpManager;

/// Sven tool that delegates execution to an external MCP server.
///
/// The `qualified_name` follows the pattern `{server_name}-{original_name}`,
/// which is unique within a sven session and compatible with the Anthropic
/// tool name regex (`[a-zA-Z0-9_-]+`).
pub struct McpTool {
    /// Qualified name sent to the model: `{server}-{original}`.
    qualified_name: String,
    /// The MCP server that owns this tool.
    server_name: String,
    /// The original tool name as returned by `tools/list`.
    original_name: String,
    /// Description (truncated to 500 chars for prompt budget management).
    description: String,
    /// Canonical JSON Schema of the tool's input.
    schema: Value,
    /// Reference to the multi-server manager for execution.
    manager: Arc<McpManager>,
}

impl McpTool {
    pub fn new(
        server_name: impl Into<String>,
        original_name: impl Into<String>,
        description: Option<String>,
        schema: Value,
        manager: Arc<McpManager>,
    ) -> Self {
        let server = server_name.into();
        let original = original_name.into();

        // Sanitize: replace anything not in [a-zA-Z0-9_-] with '_'.
        let safe_server = sanitize_tool_name_part(&server);
        let safe_original = sanitize_tool_name_part(&original);
        let qualified = format!("{safe_server}-{safe_original}");

        // Truncate description to keep the tools prompt compact.
        let desc = description.unwrap_or_default();
        let desc = if desc.len() > 500 {
            format!("{}…", &desc[..497])
        } else {
            desc
        };

        Self {
            qualified_name: qualified,
            server_name: server,
            original_name: original,
            description: desc,
            schema,
            manager,
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn original_name(&self) -> &str {
        &self.original_name
    }
}

#[async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.qualified_name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> Value {
        self.schema.clone()
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::Generic
    }

    fn is_mcp(&self) -> bool {
        true
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self
            .manager
            .call_tool(&self.server_name, &self.original_name, &call.args)
            .await
        {
            Ok(text) => ToolOutput::ok(&call.id, text),
            Err(e) => ToolOutput::err(&call.id, e.to_string()),
        }
    }
}

/// Sanitize a name fragment for use in a tool name.
///
/// Replaces any character not in `[a-zA-Z0-9_-]` with `_`.
pub fn sanitize_tool_name_part(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// An MCP prompt discovered via `prompts/list`.
///
/// Exposed as a slash command `/server_name/prompt_name` in the TUI.
#[derive(Debug, Clone)]
pub struct McpPromptInfo {
    /// Slash command path: `server_name/prompt_name`.
    pub slash_path: String,
    pub server_name: String,
    pub prompt_name: String,
    pub description: String,
    pub arguments: Vec<McpPromptArgInfo>,
}

#[derive(Debug, Clone)]
pub struct McpPromptArgInfo {
    pub name: String,
    pub description: String,
    pub required: bool,
}
