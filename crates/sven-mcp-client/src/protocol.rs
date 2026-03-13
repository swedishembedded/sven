// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! MCP JSON-RPC 2.0 wire types.
//!
//! These mirror the Model Context Protocol specification types used for
//! `initialize`, `tools/list`, `tools/call`, `prompts/list`, `prompts/get`,
//! and `resources/list`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 envelope ─────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request sent to an MCP server.
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 notification (no response expected).
#[derive(Debug, Clone, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response from an MCP server.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[allow(dead_code)]
    pub data: Option<Value>,
}

impl std::fmt::Display for JsonRpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

// ── MCP initialize ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: &'static str,
    pub capabilities: ClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientCapabilities {
    pub roots: Option<RootsCapability>,
    pub sampling: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientInfo {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: ServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: Option<ServerInfo>,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ServerCapabilities {
    pub tools: Option<Value>,
    pub prompts: Option<Value>,
    pub resources: Option<Value>,
    pub logging: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
}

pub fn initialize_params() -> InitializeParams {
    InitializeParams {
        protocol_version: "2024-11-05",
        capabilities: ClientCapabilities {
            roots: Some(RootsCapability {
                list_changed: false,
            }),
            sampling: None,
        },
        client_info: ClientInfo {
            name: "sven",
            version: env!("CARGO_PKG_VERSION"),
        },
    }
}

// ── MCP tools ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ListToolsResult {
    pub tools: Vec<McpTool>,
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

/// An MCP tool definition returned by `tools/list`.
#[derive(Debug, Clone, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// Result of a `tools/call` invocation.
#[derive(Debug, Clone, Deserialize)]
pub struct CallToolResult {
    pub content: Vec<ToolContent>,
    #[serde(rename = "isError")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    Resource {
        resource: Value,
    },
}

impl ToolContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ToolContent::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// Aggregate the content of a `CallToolResult` into a plain string.
pub fn aggregate_tool_content(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text())
        .collect::<Vec<_>>()
        .join("\n")
}

// ── MCP prompts ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ListPromptsResult {
    pub prompts: Vec<McpPrompt>,
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpPrompt {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GetPromptResult {
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromptMessage {
    pub role: String,
    pub content: PromptContent,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum PromptContent {
    Text {
        #[serde(rename = "type")]
        ty: String,
        text: String,
    },
    Other(Value),
}

impl PromptContent {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            PromptContent::Text { text, .. } => Some(text),
            PromptContent::Other(v) => v.get("text").and_then(|t| t.as_str()),
        }
    }
}

// ── MCP resources ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ListResourcesResult {
    pub resources: Vec<McpResource>,
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "mimeType")]
    pub mime_type: Option<String>,
}

// ── Tool schema canonicalization ──────────────────────────────────────────────

/// Canonicalize a JSON value by converting all objects to `BTreeMap` so key
/// order is deterministic.  This ensures that tool schema serialization
/// produces an identical byte string on every invocation, which is required
/// for Anthropic's prompt cache to generate cache hits.
pub fn canonicalize(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let ordered: BTreeMap<String, Value> =
                map.into_iter().map(|(k, v)| (k, canonicalize(v))).collect();
            Value::Object(ordered.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(canonicalize).collect()),
        other => other,
    }
}
