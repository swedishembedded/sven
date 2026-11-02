// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting agents to the tool
// servers their people use, with sign-ins that are obvious and never lost,
// for its clients. If your team needs expertise in agent tool integration
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! `/mcp add` in the TUI: where the server goes, and seeing it through to a
//! connection.

use std::sync::Arc;

/// Say where the server goes, add it there, and see it through to a
/// connection, so the person is never left wondering. A sign-in it needs is
/// shown as a link in the pager (and the browser is opened).
pub(crate) async fn add_mcp_server(
    mgr: &Arc<sven_mcp_client::McpManager>,
    toast_tx: &tokio::sync::mpsc::Sender<crate::app::ui_state::Toast>,
    notice_tx: &tokio::sync::mpsc::Sender<crate::app::Notice>,
    server: &str,
    target: &[String],
    local: bool,
) {
    use crate::app::ui_state::Toast;
    use crate::app::Notice;
    use sven_bootstrap::connectors::{self, Placement};

    let placement = connectors::current_placement(target, true, local);
    let file = sven_mcp_client::config_edit::config_file(sven_config::scope::Scope::User)
        .unwrap_or_else(|_| std::path::PathBuf::from("a configuration file"));
    let _ = toast_tx
        .send(Toast::info(connectors::adding_notice(
            server, &placement, &file,
        )))
        .await;
    let say = |text: String, copy: Option<String>| notice_tx.send(Notice { text, copy });
    match placement {
        Placement::OutdatedSignIn { origin } => {
            let _ = say(connectors::outdated_sign_in_notice(&origin), None).await;
        }
        Placement::Account(account) => {
            let Some(url) = connectors::account_target(target) else {
                return;
            };
            match connectors::connect_through_account(&account, server, url).await {
                Ok(connection) => {
                    let _ = say(
                        connectors::sign_in_notice(&connection),
                        Some(connection.sign_in_url.clone()),
                    )
                    .await;
                    sven_mcp_client::open_browser(&connection.sign_in_url);
                    // Its tools arrive once the person finishes signing in
                    // (a sign-in lasts 10 minutes).
                    let before = mgr.tool_count(connectors::SERVER).await;
                    if mgr
                        .reconnect_until_more_tools(
                            connectors::SERVER,
                            before,
                            std::time::Duration::from_secs(5),
                            std::time::Duration::from_secs(600),
                        )
                        .await
                    {
                        let _ = toast_tx
                            .send(Toast::success(format!(
                                "{} is connected",
                                connection.connector.name
                            )))
                            .await;
                    }
                }
                // The reason, in full: a toast's one line cuts it.
                Err(e) => {
                    let _ = say(format!("{e:#}"), None).await;
                }
            }
        }
        Placement::Local => {
            let added = (|| {
                let entry =
                    sven_mcp_client::config_edit::server_entry(target, &[], &[], false, None)?;
                let config: sven_mcp_client::McpServerConfig =
                    serde_yaml::from_value(entry.clone())?;
                let file = sven_mcp_client::config_edit::add(
                    sven_config::scope::Scope::User,
                    server,
                    entry,
                )?;
                anyhow::Ok((file, config))
            })();
            let (_file, config) = match added {
                Ok(added) => added,
                Err(e) => {
                    let _ = toast_tx
                        .send(Toast::error(format!(
                            "MCP server '{server}' not added: {e:#}"
                        )))
                        .await;
                    return;
                }
            };
            // A server that asks for a sign-in starts it at once; its link
            // reaches the pager as the manager's own event.
            let _ = mgr.add_server(server, config).await;
            let toast = match mgr
                .wait_connected(server, std::time::Duration::from_secs(600))
                .await
            {
                Ok(tools) => Toast::success(format!("{server} is connected ({tools} tools)")),
                Err(e) => Toast::error(format!(
                    "MCP server '{server}' is added but not connected: {e:#}. \
                     Fix that, then run /mcp auth {server} or /mcp enable {server}."
                )),
            };
            let _ = toast_tx.send(toast).await;
        }
    }
}
