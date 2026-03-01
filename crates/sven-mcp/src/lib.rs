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
//! ## Custom tool subset
//!
//! ```text
//! sven mcp serve --tools read_file,write_file,grep,run_terminal_command
//! ```
//!
//! # Architecture
//!
//! ```text
//! MCP client (Cursor, Claude Desktop, …)
//!       │  stdin/stdout (line-delimited JSON-RPC)
//!       ▼
//! SvenMcpServer (rmcp ServerHandler)
//!       │
//!       ▼
//! ToolRegistry  ──►  Tool::execute()
//! ```

pub mod bridge;
pub mod registry;
pub mod server;

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
