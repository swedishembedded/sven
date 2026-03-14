// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single-server MCP client.
//!
//! `McpConnection` wraps a [`Transport`] and handles the MCP initialization
//! handshake plus all request/response patterns.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::protocol::{
    aggregate_tool_content, canonicalize, initialize_params, CallToolResult, GetPromptResult,
    JsonRpcNotification, JsonRpcRequest, ListPromptsResult, ListResourcesResult, ListToolsResult,
    McpPrompt, McpResource, McpTool,
};
use crate::transport::Transport;

/// A live connection to a single MCP server.
pub struct McpConnection {
    transport: Transport,
    pub server_name: String,
    pub server_version: String,
    pub instructions: Option<String>,
}

impl McpConnection {
    /// Perform the MCP initialization handshake and return a connected client.
    pub async fn initialize(transport: Transport, name: impl Into<String>) -> Result<Self> {
        let name: String = name.into();
        let params = initialize_params();
        let id = transport.next_id();
        let req = JsonRpcRequest::new(id, "initialize", Some(serde_json::to_value(&params)?));

        let result = transport.send_request(&req).await.map_err(|e| {
            // Preserve UnauthorizedError without wrapping so the manager can downcast
            // and trigger the OAuth flow. Other errors get context for diagnostics.
            if e.downcast_ref::<crate::transport::UnauthorizedError>()
                .is_some()
            {
                e
            } else {
                e.context(format!("MCP initialize failed for {name}"))
            }
        })?;

        let init_result: crate::protocol::InitializeResult =
            serde_json::from_value(result).context("parse initialize result")?;

        debug!(
            server = %name,
            version = %init_result.protocol_version,
            "MCP server initialized"
        );

        // Send the initialized notification.
        let notif = JsonRpcNotification::new("notifications/initialized", None);
        transport
            .send_notification(&notif)
            .await
            .with_context(|| format!("send initialized notification to {name}"))?;

        let server_version = init_result
            .server_info
            .as_ref()
            .map(|s| format!("{} {}", s.name, s.version))
            .unwrap_or_default();

        info!(
            server = %name,
            version = %server_version,
            "MCP server connected"
        );

        Ok(Self {
            transport,
            server_name: name,
            server_version,
            instructions: init_result.instructions,
        })
    }

    // ── Tools ─────────────────────────────────────────────────────────────────

    /// Fetch the tool list from the server.
    pub async fn list_tools(&self) -> Result<Vec<McpTool>> {
        let id = self.transport.next_id();
        let req = JsonRpcRequest::new(id, "tools/list", None);
        let result = self
            .transport
            .send_request(&req)
            .await
            .context("tools/list")?;

        let list: ListToolsResult =
            serde_json::from_value(result).context("parse tools/list result")?;

        // Canonicalize the input_schema for each tool to ensure deterministic
        // JSON serialization (required for Anthropic prompt cache stability).
        let tools = list
            .tools
            .into_iter()
            .map(|mut t| {
                t.input_schema = canonicalize(t.input_schema);
                t
            })
            .collect();

        Ok(tools)
    }

    /// Call a tool by its original (non-qualified) name.
    pub async fn call_tool(&self, name: &str, args: &Value) -> Result<String> {
        let id = self.transport.next_id();
        let params = json!({
            "name": name,
            "arguments": args,
        });
        let req = JsonRpcRequest::new(id, "tools/call", Some(params));
        let result = self
            .transport
            .send_request(&req)
            .await
            .with_context(|| format!("tools/call {name}"))?;

        let call_result: CallToolResult =
            serde_json::from_value(result).context("parse tools/call result")?;

        let is_error = call_result.is_error.unwrap_or(false);
        let text = aggregate_tool_content(&call_result);

        if is_error {
            Err(anyhow::anyhow!("MCP tool {name} returned error: {text}"))
        } else {
            Ok(text)
        }
    }

    // ── Prompts ───────────────────────────────────────────────────────────────

    /// Fetch the prompt list from the server.
    pub async fn list_prompts(&self) -> Result<Vec<McpPrompt>> {
        let id = self.transport.next_id();
        let req = JsonRpcRequest::new(id, "prompts/list", None);
        let result = self
            .transport
            .send_request(&req)
            .await
            .context("prompts/list")?;

        let list: ListPromptsResult =
            serde_json::from_value(result).context("parse prompts/list result")?;

        Ok(list.prompts)
    }

    /// Get a prompt by name, passing the given arguments.
    pub async fn get_prompt(&self, name: &str, args: &HashMap<String, String>) -> Result<String> {
        let id = self.transport.next_id();
        let params = json!({
            "name": name,
            "arguments": args,
        });
        let req = JsonRpcRequest::new(id, "prompts/get", Some(params));
        let result = self
            .transport
            .send_request(&req)
            .await
            .with_context(|| format!("prompts/get {name}"))?;

        let prompt_result: GetPromptResult =
            serde_json::from_value(result).context("parse prompts/get result")?;

        let text = prompt_result
            .messages
            .iter()
            .filter_map(|m| m.content.as_text())
            .collect::<Vec<_>>()
            .join("\n\n");

        Ok(text)
    }

    // ── Resources ─────────────────────────────────────────────────────────────

    /// Fetch the resource list from the server.
    pub async fn list_resources(&self) -> Result<Vec<McpResource>> {
        let id = self.transport.next_id();
        let req = JsonRpcRequest::new(id, "resources/list", None);
        let result = self
            .transport
            .send_request(&req)
            .await
            .context("resources/list")?;

        let list: ListResourcesResult =
            serde_json::from_value(result).context("parse resources/list result")?;

        Ok(list.resources)
    }

    // ── Ping ──────────────────────────────────────────────────────────────────

    /// Send a ping to check if the server is still alive.
    pub async fn ping(&self) -> Result<()> {
        let id = self.transport.next_id();
        let req = JsonRpcRequest::new(id, "ping", None);
        self.transport.send_request(&req).await.context("ping")?;
        Ok(())
    }

    /// Whether this connection uses an HTTP transport.
    pub fn is_http(&self) -> bool {
        self.transport.is_http()
    }
}
