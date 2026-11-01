// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between MCP tool definitions and sven's `Tool` trait.
//!
//! `McpTool` wraps an MCP tool definition and routes execution through the
//! `McpManager`.  It integrates transparently with `sven_tool_registry::ToolRegistry`.

use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};
use serde_json::Value;

use sven_tool_api::{
    ApprovalPolicy, OutputCategory, Tool, ToolCall, ToolCapability, ToolOutput, ToolOutputPart,
};

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
    /// The description's first sentence, for the index of tools not loaded.
    summary: String,
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
        let desc = if desc.chars().count() > 500 {
            let head: String = desc.chars().take(497).collect();
            format!("{head}...")
        } else {
            desc
        };
        let summary = sven_tool_api::one_line_summary(&desc, sven_tool_api::SUMMARY_MAX_CHARS);

        Self {
            qualified_name: qualified,
            server_name: server,
            original_name: original,
            description: desc,
            summary,
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

    fn summary(&self) -> &str {
        &self.summary
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
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::NetworkAccess
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self
            .manager
            .call_tool(&self.server_name, &self.original_name, &call.args)
            .await
        {
            Ok(result) => tool_output(&call.id, result),
            Err(e) => ToolOutput::err(&call.id, format!("{e:#}")),
        }
    }
}

/// An MCP tool result as sven's tool output, nothing dropped: text, images
/// and audio as themselves, an embedded resource as its text (or a note
/// naming a binary one), a resource link as its URI, and the structured
/// result as JSON when the server sent no content to read it from.
pub fn tool_output(call_id: &str, result: CallToolResult) -> ToolOutput {
    let mut parts: Vec<ToolOutputPart> = result.content.into_iter().map(content_part).collect();
    if parts.is_empty() {
        if let Some(structured) = &result.structured_content {
            parts.push(ToolOutputPart::Text(structured.to_string()));
        }
    }
    let mut output = ToolOutput::with_parts(call_id, parts);
    output.is_error = result.is_error.unwrap_or(false);
    output
}

fn content_part(block: ContentBlock) -> ToolOutputPart {
    match block {
        ContentBlock::Text(t) => ToolOutputPart::Text(t.text),
        ContentBlock::Image(i) => {
            ToolOutputPart::Image(format!("data:{};base64,{}", i.mime_type, i.data))
        }
        ContentBlock::Audio(a) => {
            ToolOutputPart::Audio(format!("data:{};base64,{}", a.mime_type, a.data))
        }
        ContentBlock::Resource(r) => match r.resource {
            ResourceContents::TextResourceContents { uri, text, .. } => {
                ToolOutputPart::Text(format!("[resource {uri}]\n{text}"))
            }
            ResourceContents::BlobResourceContents {
                uri,
                mime_type,
                blob,
                ..
            } => match mime_type.as_deref() {
                Some(mime) if mime.starts_with("image/") => {
                    ToolOutputPart::Image(format!("data:{mime};base64,{blob}"))
                }
                Some(mime) if mime.starts_with("audio/") => {
                    ToolOutputPart::Audio(format!("data:{mime};base64,{blob}"))
                }
                mime => ToolOutputPart::Text(format!(
                    "[binary resource {uri} ({}), {} bytes base64]",
                    mime.unwrap_or("application/octet-stream"),
                    blob.len()
                )),
            },
            _ => ToolOutputPart::Text("[a resource of a kind sven does not read]".to_string()),
        },
        ContentBlock::ResourceLink(link) => ToolOutputPart::Text(match &link.description {
            Some(description) => format!(
                "[resource link {} ({}): {description}]",
                link.uri, link.name
            ),
            None => format!("[resource link {} ({})]", link.uri, link.name),
        }),
        _ => ToolOutputPart::Text("[content of a kind sven does not read]".to_string()),
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
