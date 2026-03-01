// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! `sven-mcp` — MCP (Model Context Protocol) server for sven.
//!
//! Exposes sven's built-in tools to any MCP-compatible host (Cursor, Claude
//! Desktop, opencode, codex, etc.) over **stdio** transport using
//! line-delimited JSON-RPC.
//!
//! # Quick start
//!
//! ```text
//! sven mcp serve
//! ```
//!
//! # MCP client configuration
//!
//! ## Cursor / Claude Desktop (`mcp.json`)
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "sven": {
//!       "command": "sven",
//!       "args": ["mcp", "serve"]
//!     }
//!   }
//! }
//! ```
//!
//! ## Proxy to a running sven node (all node tools available)
//!
//! ```json
//! {
//!   "mcpServers": {
//!     "sven": {
//!       "command": "sven",
//!       "args": ["mcp", "serve", "--node-url", "wss://127.0.0.1:18790/ws", "--token", "<token>"]
//!     }
//!   }
//! }
//! ```
//!
//! ## Custom tool subset (local mode)
//!
//! ```text
//! sven mcp serve --tools read_file,write_file,grep,run_terminal_command
//! ```
//!
//! # Architecture
//!
//! ## Local mode
//! ```text
//! MCP client (Cursor, Claude Desktop, …)
//!       │  stdin/stdout (line-delimited JSON-RPC)
//!       ▼
//! SvenMcpServer (rmcp ServerHandler)
//!       │
//!       ▼
//! ToolRegistry  ──►  Tool::execute()
//! ```
//!
//! ## Node-proxy mode
//! ```text
//! MCP client (Cursor, Claude Desktop, …)
//!       │  stdin/stdout (line-delimited JSON-RPC)
//!       ▼
//! NodeProxyServer (rmcp ServerHandler)
//!       │  wss:// (ListTools / CallTool commands)
//!       ▼
//! sven node (full tool registry incl. list_peers, delegate_task)
//! ```

pub mod bridge;
pub mod node_proxy;
pub mod registry;
pub mod server;

pub use node_proxy::NodeProxyServer;
pub use registry::{build_mcp_registry, DEFAULT_TOOL_NAMES};
pub use server::SvenMcpServer;

use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use sven_tools::ToolRegistry;

/// Start an MCP stdio server, serving the tools in `registry` on
/// `stdin` / `stdout`.
///
/// This function blocks until the client disconnects (stdin EOF) or the
/// process is terminated.  It is designed to be called as the sole operation
/// of the `sven mcp serve` subcommand.
///
/// # Errors
///
/// Returns an error if the rmcp transport fails to initialize or if the
/// server encounters a fatal I/O error.
pub async fn serve_stdio(registry: Arc<ToolRegistry>) -> Result<()> {
    let server = SvenMcpServer::new(registry);
    let running = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| anyhow::anyhow!("MCP server init error: {e}"))?;
    running
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP server error: {e}"))?;
    Ok(())
}

/// Start an MCP stdio server that **proxies** every tool call to a running
/// `sven node` over WebSocket.
///
/// `ws_url` must point to the node's `/ws` endpoint, e.g.
/// `wss://127.0.0.1:18790/ws`.  `token` is the raw bearer token printed by
/// `sven node start` on first launch.
///
/// All tools registered on the node — including P2P tools like `list_peers`
/// and `delegate_task` — are exposed to the MCP client transparently.
///
/// This function blocks until stdin closes or a fatal error occurs.
pub async fn serve_stdio_node_proxy(ws_url: String, token: String) -> Result<()> {
    let server = NodeProxyServer::new(ws_url, token);
    let running = server
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .map_err(|e| anyhow::anyhow!("MCP node-proxy server init error: {e}"))?;
    running
        .waiting()
        .await
        .map_err(|e| anyhow::anyhow!("MCP node-proxy server error: {e}"))?;
    Ok(())
}
