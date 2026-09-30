// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! ACP (Agent Client Protocol) server for sven.
//!
//! Exposes one entry point, [`serve_stdio`], which starts a local sven agent
//! in-process and speaks ACP over stdin/stdout. It follows the same structure
//! as `sven-mcp::serve_stdio` and produces log output to stderr only
//! (stdin/stdout are reserved for the JSON-RPC framing).
//!
//! ## Usage (IDE config)
//!
//! ```json
//! { "agents": { "sven": { "command": "sven", "args": ["acp", "serve"] } } }
//! ```

pub mod bridge;
pub mod cli;

mod agent;

use std::sync::Arc;

use agent_client_protocol::{
    AgentSideConnection, Client, RequestPermissionOutcome, RequestPermissionResponse,
};
use anyhow::Result;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::debug;

use sven_config::Config;

use agent::{ConnMessage, SvenAcpAgent};

// ─── Public API ───────────────────────────────────────────────────────────────

/// Start an ACP server backed by a local sven agent, communicating over stdio.
///
/// The function blocks until stdin reaches EOF (i.e. the IDE disconnects the
/// subprocess).  All ACP framing happens over stdin/stdout; tracing is written
/// to stderr.
pub async fn serve_stdio(config: Arc<Config>) -> Result<()> {
    serve_stdio_with(config, Some(agent::DEFAULT_PERMISSION_TIMEOUT)).await
}

/// [`serve_stdio`] with a chosen wait for the client's permission answers:
/// `None` waits for each answer however long the client takes.
pub async fn serve_stdio_with(
    config: Arc<Config>,
    permission_timeout: Option<std::time::Duration>,
) -> Result<()> {
    debug!("Starting ACP local server");

    let (conn_tx, mut conn_rx) = tokio::sync::mpsc::unbounded_channel::<ConnMessage>();
    let acp_agent = SvenAcpAgent::new(config, conn_tx).with_permission_timeout(permission_timeout);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let outgoing = tokio::io::stdout().compat_write();
            let incoming = tokio::io::stdin().compat();

            let (conn, handle_io) =
                AgentSideConnection::new(acp_agent, outgoing, incoming, |fut| {
                    tokio::task::spawn_local(fut);
                });

            // Background task: forward session updates and permission requests
            // from the agent to conn.
            tokio::task::spawn_local(async move {
                while let Some(msg) = conn_rx.recv().await {
                    match msg {
                        ConnMessage::SessionUpdate(notification, ack_tx) => {
                            conn.session_notification(notification).await.ok();
                            ack_tx.send(()).ok();
                        }
                        ConnMessage::RequestPermission {
                            request,
                            response_tx,
                        } => {
                            let response =
                                conn.request_permission(request).await.unwrap_or_else(|_| {
                                    RequestPermissionResponse::new(
                                        RequestPermissionOutcome::Cancelled,
                                    )
                                });
                            response_tx.send(response).ok();
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
