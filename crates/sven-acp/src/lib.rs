// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! ACP (Agent Client Protocol) server for sven.
//!
//! Exposes two entry points:
//!
//! * [`serve_stdio`] — starts a local sven agent in-process and speaks ACP
//!   over stdin/stdout.
//! * [`serve_stdio_node_proxy`] — proxies ACP requests to a running `sven node`
//!   over WebSocket.
//!
//! Both entry points follow the same structure as `sven-mcp::serve_stdio[_node_proxy]`
//! and produce log output to stderr only (stdin/stdout are reserved for the
//! JSON-RPC framing).
//!
//! ## Usage (IDE config)
//!
//! ```json
//! { "agents": { "sven": { "command": "sven", "args": ["acp", "serve"] } } }
//! ```

pub mod bridge;

mod agent;
mod node_proxy;

use std::sync::Arc;

use agent_client_protocol::{AgentSideConnection, Client};
use anyhow::Result;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::debug;

use sven_config::Config;

use agent::{ConnMessage, SvenAcpAgent};
use node_proxy::SvenAcpNodeProxy;

// ─── Public API ───────────────────────────────────────────────────────────────

/// Start an ACP server backed by a local sven agent, communicating over stdio.
///
/// The function blocks until stdin reaches EOF (i.e. the IDE disconnects the
/// subprocess).  All ACP framing happens over stdin/stdout; tracing is written
/// to stderr.
pub async fn serve_stdio(config: Arc<Config>) -> Result<()> {
    debug!("Starting ACP local server");

    let (conn_tx, mut conn_rx) = tokio::sync::mpsc::unbounded_channel::<ConnMessage>();
    let acp_agent = SvenAcpAgent::new(config, conn_tx);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let outgoing = tokio::io::stdout().compat_write();
            let incoming = tokio::io::stdin().compat();

            let (conn, handle_io) =
                AgentSideConnection::new(acp_agent, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            // Background task: forward session notifications from the agent to conn.
            tokio::task::spawn_local(async move {
                while let Some(msg) = conn_rx.recv().await {
                    match msg {
                        ConnMessage::SessionUpdate(notification, ack_tx) => {
                            conn.session_notification(notification).await.ok();
                            ack_tx.send(()).ok();
                        }
                    }
                }
            });

            handle_io
                .await
                .map_err(|e| anyhow::anyhow!("ACP I/O error: {e}"))
        })
        .await
}

/// Start an ACP server that proxies all requests to a running `sven node`
/// over WebSocket.
///
/// `ws_url` — the WebSocket URL of the node (e.g. `wss://127.0.0.1:18790/ws`)
/// `token`  — bearer token printed by `sven node start`
pub async fn serve_stdio_node_proxy(ws_url: String, token: String) -> Result<()> {
    debug!("Starting ACP node-proxy server, node={ws_url}");

    let (conn_tx, mut conn_rx) = tokio::sync::mpsc::unbounded_channel::<ConnMessage>();
    let proxy = SvenAcpNodeProxy::new(ws_url, token, conn_tx);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let outgoing = tokio::io::stdout().compat_write();
            let incoming = tokio::io::stdin().compat();

            let (conn, handle_io) = AgentSideConnection::new(proxy, outgoing, incoming, |fut| {
                tokio::task::spawn_local(fut);
            });

            tokio::task::spawn_local(async move {
                while let Some(msg) = conn_rx.recv().await {
                    match msg {
                        ConnMessage::SessionUpdate(notification, ack_tx) => {
                            conn.session_notification(notification).await.ok();
                            ack_tx.send(()).ok();
                        }
                    }
                }
            });

            handle_io
                .await
                .map_err(|e| anyhow::anyhow!("ACP I/O error: {e}"))
        })
        .await
}
