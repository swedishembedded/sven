// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! [`SvenMcpServer`] — the rmcp [`ServerHandler`] implementation.
//!
//! This struct wraps a sven [`ToolRegistry`] and implements the MCP
//! `tools/list` and `tools/call` protocol methods.  All other MCP lifecycle
//! methods (initialize, shutdown, ping) are handled by the default rmcp
//! implementations.
//!
//! The server is stateless: every `call_tool` request executes the tool in
//! isolation and does not carry any session state between calls.  This matches
//! the typical expectations of an MCP client (Cursor, Claude Desktop, etc.)
//! that manages its own conversation context.

use std::sync::Arc;

use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError,
};
use sven_tools::{ToolCall, ToolRegistry};
use uuid::Uuid;

use crate::bridge::{output_to_call_result, schema_to_mcp_tool};

/// Sven MCP server — wraps a [`ToolRegistry`] and speaks the MCP protocol.
///
/// Create with [`SvenMcpServer::new`] and then call [`rmcp::ServiceExt::serve`]
/// to start serving on a transport.
#[derive(Clone)]
pub struct SvenMcpServer {
    registry: Arc<ToolRegistry>,
}

impl SvenMcpServer {
    /// Create a new server backed by the given [`ToolRegistry`].
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

impl ServerHandler for SvenMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..ServerInfo::default()
        }
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        let registry = self.registry.clone();
        async move {
            let tools = registry
                .schemas()
                .into_iter()
                .map(schema_to_mcp_tool)
                .collect();
            Ok(ListToolsResult {
                tools,
                next_cursor: None,
                meta: None,
            })
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let args = request
            .arguments
            .map(|m| serde_json::Value::Object(m.into_iter().collect()))
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

        let call = ToolCall {
            id: Uuid::new_v4().to_string(),
            name: request.name.to_string(),
            args,
        };

        let output = self.registry.execute(&call).await;
        Ok(output_to_call_result(output))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
//
// These tests cover the parts of SvenMcpServer that can be tested without
// an active transport or RequestContext.  The full list_tools / call_tool
// round-trips are covered by the integration tests in tests/integration.rs.

#[cfg(test)]
mod tests {
    use super::*;
    use sven_tools::ToolRegistry;

    fn make_server_with(tools: impl FnOnce(&mut ToolRegistry)) -> SvenMcpServer {
        let mut reg = ToolRegistry::new();
        tools(&mut reg);
        SvenMcpServer::new(Arc::new(reg))
    }

    // ── get_info ──────────────────────────────────────────────────────────

    #[test]
    fn get_info_enables_tools_capability() {
        let server = make_server_with(|_| {});
        let info = server.get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "tools capability must be enabled"
        );
    }

    #[test]
    fn get_info_has_no_resources_capability_by_default() {
        let server = make_server_with(|_| {});
        let info = server.get_info();
        // sven only exposes tools; resources and prompts are not supported.
        assert!(info.capabilities.resources.is_none());
        assert!(info.capabilities.prompts.is_none());
    }

    // ── SvenMcpServer construction ────────────────────────────────────────

    #[test]
    fn server_is_cloneable() {
        let server = make_server_with(|_| {});
        let _clone = server.clone();
    }

    #[test]
    fn empty_registry_server_reports_no_tools_in_schema() {
        let server = make_server_with(|_| {});
        assert!(server.registry.schemas().is_empty());
    }
}
