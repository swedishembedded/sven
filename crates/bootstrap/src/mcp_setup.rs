// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting LLM agents to
// external tool servers for its clients. If your team needs expertise in MCP
// integration then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Connecting a session to its configured MCP servers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sven_mcp_client::{McpEvent, McpManager};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// What a session needs to obtain its MCP manager.
pub(crate) struct McpSetup<'a> {
    /// A manager the caller already runs (a session rebuild): reused so the
    /// frontend's handle and the MCP connections survive a mid-session mode
    /// or model change.
    pub shared: Option<Arc<McpManager>>,
    /// No built-in tools means no configured MCP servers either: the caller
    /// asked for exactly the tools it registers.
    pub no_builtin_tools: bool,
    pub servers: &'a HashMap<String, sven_mcp_client::McpServerConfig>,
    pub allow_interactive_oauth: bool,
    /// Wait up to this long for the first MCP tool to appear.
    pub wait_for_tools_ms: Option<u64>,
}

/// The manager and its event stream. A reused manager yields a closed
/// receiver stub: the caller keeps consuming from the original manager's
/// stream.
///
/// # Errors
///
/// A server the configuration marks required did not connect and offer a
/// tool within its timeout: the session must not start without it.
pub(crate) async fn connect(
    setup: McpSetup<'_>,
) -> anyhow::Result<(Arc<McpManager>, mpsc::Receiver<McpEvent>)> {
    // Reuse a caller-supplied manager (session rebuild) instead of building
    // and connecting a fresh one, so the frontend's manager handle and MCP
    // connections survive a mid-session mode/model change. A reused manager
    // yields a closed event-receiver stub — the caller keeps consuming from
    // the original manager's stream.
    match setup.shared {
        Some(existing) => {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok((existing, rx))
        }
        // No built-in tools means no configured MCP servers either: the
        // caller asked for exactly the tools it registers.
        None if setup.no_builtin_tools => {
            let (mcp_event_tx, mcp_event_rx) = tokio::sync::mpsc::channel(1);
            let manager = McpManager::new(Default::default(), mcp_event_tx, false);
            Ok((manager, mcp_event_rx))
        }
        None => {
            let (mcp_event_tx, mcp_event_rx) = tokio::sync::mpsc::channel(64);
            let mcp_manager = McpManager::new(
                setup.servers.clone(),
                mcp_event_tx,
                setup.allow_interactive_oauth,
            );
            mcp_manager.connect_all().await;
            mcp_manager.start_background_tasks();

            let has_enabled_servers = setup.servers.values().any(|c| c.enabled);
            if let Some(timeout_ms) = setup.wait_for_tools_ms {
                if has_enabled_servers {
                    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
                    let poll_interval = Duration::from_millis(200);
                    loop {
                        let tools = mcp_manager.tools().await;
                        if !tools.is_empty() {
                            info!(
                                count = tools.len(),
                                "MCP tools available for kernel runtime"
                            );
                            break;
                        }
                        if tokio::time::Instant::now() >= deadline {
                            warn!(timeout_ms, "MCP tools not available within timeout");
                            break;
                        }
                        tokio::time::sleep(poll_interval).await;
                    }
                }
            }
            require_servers(&mcp_manager, setup.servers).await?;
            Ok((mcp_manager, mcp_event_rx))
        }
    }
}

/// Waits for every server marked required to connect and offer a tool,
/// each within its own timeout, and fails naming the first that did not.
async fn require_servers(
    manager: &Arc<McpManager>,
    servers: &HashMap<String, sven_mcp_client::McpServerConfig>,
) -> anyhow::Result<()> {
    for (name, config) in servers.iter().filter(|(_, c)| c.enabled && c.required) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(config.timeout_secs);
        loop {
            let ready = manager
                .server_statuses()
                .await
                .into_iter()
                .find(|s| &s.name == name)
                .is_some_and(|s| {
                    matches!(s.status, sven_mcp_client::ServerStatus::Connected) && s.tool_count > 0
                });
            if ready {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "the required MCP server `{name}` did not start and offer a tool within {}s",
                    config.timeout_secs
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_mcp_client::{McpServerConfig, McpTransport};

    fn servers(required: bool) -> HashMap<String, McpServerConfig> {
        let config = McpServerConfig {
            transport: McpTransport::Stdio {
                command: "true".into(),
                args: Vec::new(),
            },
            timeout_secs: 2,
            required,
            ..McpServerConfig::default()
        };
        [("toolkit".to_string(), config)].into_iter().collect()
    }

    fn setup(servers: &HashMap<String, McpServerConfig>) -> McpSetup<'_> {
        McpSetup {
            shared: None,
            no_builtin_tools: false,
            servers,
            allow_interactive_oauth: false,
            wait_for_tools_ms: None,
        }
    }

    #[tokio::test]
    async fn a_session_does_not_start_without_a_required_server() {
        let required = servers(true);
        let error = connect(setup(&required)).await.err().expect("refused");
        assert!(
            format!("{error:#}").contains("required MCP server `toolkit`"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_server_that_is_not_required_never_stops_a_session() {
        let optional = servers(false);
        assert!(connect(setup(&optional)).await.is_ok());
    }
}
