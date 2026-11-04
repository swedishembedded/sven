// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//!
//! `sven-mcp` - MCP (Model Context Protocol) server for sven.
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
//! ## Custom tool subset (local mode)
//!
//! ```text
//! sven mcp serve --tools read_file,write_file,grep,run_terminal_command
//! ```
//!
//! # Architecture
//!
//! ```text
//! MCP client (Cursor, Claude Desktop, ...)
//!       │  stdin/stdout (line-delimited JSON-RPC)
//!       ▼
//! SvenMcpServer (rmcp ServerHandler)
//!       │
//!       ▼
//! ToolRegistry  ──►  Tool::execute()
//! ```

pub mod bridge;
pub mod cli;
pub mod registry;
pub mod server;

pub use registry::{
    build_mcp_registry, check_requested_tools, DEFAULT_KEYWORD, DEFAULT_TOOL_NAMES,
    OPT_IN_TOOL_NAMES,
};
pub use server::SvenMcpServer;

use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use sven_tool_registry::ToolRegistry;

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

/// The shortest bearer token `serve_http` accepts.
pub const MIN_TOKEN_CHARS: usize = 32;

/// Serves `registry` over Streamable HTTP at `/mcp` on `listener`, to a
/// client that presents `token` as its bearer token and to nobody else.
/// Meant for a host that keeps its plan in one place and runs tools in
/// another (a sandbox, a verifier) it reaches over a private network.
///
/// # Errors
///
/// A token shorter than [`MIN_TOKEN_CHARS`].
pub async fn serve_http(
    registry: Arc<ToolRegistry>,
    listener: tokio::net::TcpListener,
    token: String,
) -> Result<()> {
    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Full};
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
    use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
    use subtle::ConstantTimeEq;
    use tower_service::Service as _;

    anyhow::ensure!(
        token.chars().count() >= MIN_TOKEN_CHARS,
        "the token must be at least {MIN_TOKEN_CHARS} characters"
    );
    let expected = Arc::new(format!("Bearer {token}"));
    let service = StreamableHttpService::new(
        move || Ok(SvenMcpServer::new(registry.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    loop {
        let (stream, _) = listener.accept().await?;
        let (service, expected) = (service.clone(), Arc::clone(&expected));
        let handle =
            hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let (mut service, expected) = (service.clone(), Arc::clone(&expected));
                async move {
                    type Body = BoxBody<Bytes, std::convert::Infallible>;
                    let refuse = |status: u16| -> hyper::Response<Body> {
                        hyper::Response::builder()
                            .status(status)
                            .header("www-authenticate", "Bearer")
                            .body(Full::new(Bytes::new()).boxed())
                            .unwrap_or_default()
                    };
                    let presented = req
                        .headers()
                        .get("authorization")
                        .map(|v| v.as_bytes().to_vec())
                        .unwrap_or_default();
                    let allowed: bool = presented.ct_eq(expected.as_bytes()).into();
                    if !allowed {
                        return Ok::<_, std::convert::Infallible>(refuse(401));
                    }
                    if req.uri().path() != "/mcp" {
                        return Ok(refuse(404));
                    }
                    match service.call(req).await {
                        Ok(response) => Ok(response.map(|body| body.boxed())),
                        Err(_) => Ok(refuse(500)),
                    }
                }
            });
        tokio::spawn(async move {
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), handle)
                .await;
        });
    }
}
