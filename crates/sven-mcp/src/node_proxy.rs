// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! MCP server that proxies tool calls to a running `sven node` over WebSocket.
//!
//! # How it works
//!
//! Each `tools/list` and `tools/call` operation opens a fresh authenticated
//! WebSocket connection to the node's `/ws` endpoint, issues a single
//! [`ControlCommand`] (`ListTools` or `CallTool`), waits for the matching
//! [`ControlEvent`] response, then closes the connection.
//!
//! The approach is intentionally simple: MCP tool calls are rare enough that
//! per-request connections are fine.  The `call_id` in `CallTool` / `ToolCallOutput`
//! ensures the right response is matched even if other events arrive first.
//!
//! # Authentication
//!
//! The bearer token is sent as an `Authorization: Bearer <token>` HTTP header
//! during the WebSocket upgrade handshake, exactly as the `sven node` expects.
//!
//! # TLS
//!
//! Accepts self-signed certificates when the URL scheme is `wss://`.  This is
//! appropriate because the node's TLS certificate is operator-controlled and
//! the token provides the actual authentication guarantee.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use rmcp::{
    handler::server::ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, Content, ListToolsResult, PaginatedRequestParams,
        ServerCapabilities, ServerInfo, Tool as McpTool,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::warn;
use uuid::Uuid;

// ── Wire types (mirrors sven-node control protocol) ───────────────────────────

/// Subset of `ControlCommand` serialised as JSON for the WebSocket.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsCommand {
    ListTools,
    CallTool {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
}

/// Subset of `ControlEvent` that `NodeProxyServer` cares about.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WsEvent {
    ToolList {
        tools: Vec<WsToolSchemaInfo>,
    },
    ToolCallOutput {
        call_id: String,
        output: String,
        is_error: bool,
    },
    GatewayError {
        code: u32,
        message: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct WsToolSchemaInfo {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ── NodeProxyServer ───────────────────────────────────────────────────────────

/// MCP `ServerHandler` that proxies every tool call to a live `sven node`.
///
/// Construct via [`NodeProxyServer::new`] and start the MCP stdio server with
/// [`crate::serve_stdio_node_proxy`].
#[derive(Clone)]
pub struct NodeProxyServer {
    /// WebSocket URL of the node, e.g. `wss://127.0.0.1:18790/ws`.
    ws_url: Arc<String>,
    /// Raw bearer token (not the hash).
    token: Arc<String>,
}

impl NodeProxyServer {
    pub fn new(ws_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            ws_url: Arc::new(ws_url.into()),
            token: Arc::new(token.into()),
        }
    }

    async fn connect(&self) -> Result<sven_node_client::NodeWsStream> {
        sven_node_client::connect(&self.ws_url, &self.token).await
    }

    /// Send a [`WsCommand`] and collect the first matching [`WsEvent`].
    async fn roundtrip<F>(&self, cmd: WsCommand, mut matcher: F) -> Result<WsEvent>
    where
        F: FnMut(&WsEvent) -> bool,
    {
        let mut ws = self.connect().await?;
        sven_node_client::send_json(&mut ws, &cmd).await?;

        while let Some(msg) = ws.next().await {
            let msg: WsMessage = msg.context("WebSocket read error")?;
            let text = match msg {
                WsMessage::Text(t) => t,
                WsMessage::Close(_) => bail!("node closed the connection unexpectedly"),
                _ => continue,
            };

            let event: WsEvent = match serde_json::from_str(&text) {
                Ok(ev) => ev,
                Err(e) => {
                    warn!("unparseable event from node: {e} — {text}");
                    continue;
                }
            };

            if let WsEvent::GatewayError { code, ref message } = event {
                bail!("node error {code}: {message}");
            }

            if matcher(&event) {
                let _ = ws.close(None).await;
                return Ok(event);
            }
        }

        bail!("WebSocket closed before receiving expected response")
    }

    async fn fetch_tool_list(&self) -> Result<Vec<WsToolSchemaInfo>> {
        let event = self
            .roundtrip(WsCommand::ListTools, |ev| {
                matches!(ev, WsEvent::ToolList { .. })
            })
            .await?;

        if let WsEvent::ToolList { tools } = event {
            Ok(tools)
        } else {
            bail!("unexpected event type for list_tools")
        }
    }

    async fn execute_tool(&self, name: String, args: serde_json::Value) -> Result<(String, bool)> {
        let call_id = Uuid::new_v4().to_string();
        let cid = call_id.clone();

        let event = self
            .roundtrip(
                WsCommand::CallTool {
                    call_id,
                    name,
                    args,
                },
                move |ev| {
                    if let WsEvent::ToolCallOutput { call_id, .. } = ev {
                        call_id == &cid
                    } else {
                        false
                    }
                },
            )
            .await?;

        if let WsEvent::ToolCallOutput {
            output, is_error, ..
        } = event
        {
            Ok((output, is_error))
        } else {
            bail!("unexpected event type for call_tool")
        }
    }
}

// ── rmcp ServerHandler impl ───────────────────────────────────────────────────

impl ServerHandler for NodeProxyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            ..ServerInfo::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let schemas = self.fetch_tool_list().await.map_err(|e| {
            tracing::error!(url = %self.ws_url, "list_tools proxy failed: {e:#}");
            McpError {
                code: rmcp::model::ErrorCode::INTERNAL_ERROR,
                message: e.to_string().into(),
                data: None,
            }
        })?;

        let tools: Vec<McpTool> = schemas
            .into_iter()
            .map(|s| {
                let input_schema: rmcp::model::JsonObject =
                    match serde_json::from_value(s.parameters) {
                        Ok(obj) => obj,
                        Err(_) => serde_json::Map::new(),
                    };
                McpTool::new(
                    std::borrow::Cow::Owned(s.name),
                    std::borrow::Cow::Owned(s.description),
                    Arc::new(input_schema),
                )
            })
            .collect();

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
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

        let (output, is_error) = self
            .execute_tool(request.name.to_string(), args)
            .await
            .map_err(|e| {
                tracing::error!(url = %self.ws_url, tool = %request.name, "call_tool proxy failed: {e:#}");
                McpError {
                    code: rmcp::model::ErrorCode::INTERNAL_ERROR,
                    message: e.to_string().into(),
                    data: None,
                }
            })?;

        let content = vec![Content::text(output)];
        if is_error {
            Ok(CallToolResult {
                content,
                is_error: Some(true),
                structured_content: None,
                meta: None,
            })
        } else {
            Ok(CallToolResult::success(content))
        }
    }
}
