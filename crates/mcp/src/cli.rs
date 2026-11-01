// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven mcp` clap grammar and handler, reusable both by the monolithic
//! `sven` binary's `Commands::Mcp` dispatch and the standalone `sven-mcp`
//! binary.

use clap::Subcommand;

/// `sven mcp` subcommands.
#[derive(Subcommand, Debug)]
pub enum McpCommands {
    /// Expose sven as an MCP server over stdio.
    ///
    /// Starts a Model Context Protocol server that speaks line-delimited
    /// JSON-RPC on stdin/stdout.  Any MCP-compatible host can launch sven
    /// as a subprocess and call its tools:
    ///
    ///   Cursor / Claude Desktop / opencode (`mcp.json`):
    ///
    ///   { "mcpServers": { "sven": { "command": "sven", "args": ["mcp", "serve"] } } }
    ///
    /// The server blocks until stdin reaches EOF (i.e. until the host
    /// disconnects).  It does not fork, does not bind a port, and requires
    /// no authentication - security is inherited from the host process.
    ///
    /// Every served tool runs when the host calls it: the host applies its
    /// own permission UI. The file tools and the shell's working directory
    /// are confined to the directory the server starts in, and the shell
    /// refuses a command matching `tools.deny_patterns`.
    Serve {
        /// Comma-separated list of tool names to expose.
        ///
        /// Defaults to the MCP-safe built-in tools (see
        /// `sven_mcp::DEFAULT_TOOL_NAMES`); pass `default` to say so
        /// explicitly. Tools that need a live session (`ask_question`,
        /// `todo`) or change agent state are not offered over MCP. An
        /// unknown name is an error.
        ///
        /// Example: --tools read_file,write_file,grep
        #[arg(long, value_name = "TOOL,...")]
        tools: Option<String>,

        /// Brave Search API key for the web_search tool.
        ///
        /// May also be provided via the BRAVE_API_KEY environment variable.
        #[arg(long, env = "BRAVE_API_KEY", value_name = "KEY")]
        brave_api_key: Option<String>,
    },

    /// List the MCP servers sven is configured to use (`mcp_servers:`), each
    /// with whether it connects, how many tools it offers, and whether it
    /// needs a sign-in.
    List,

    /// Sign in to an OAuth-protected MCP server, from any terminal.
    ///
    /// Opens the sign-in page in the browser and prints its address. When
    /// the browser runs on another machine (a remote shell, a container) it
    /// cannot come back to sven by itself: paste the address it ends on
    /// here instead. The sign-in is kept for every later sven run.
    Auth {
        /// The server's name in `mcp_servers:`.
        server: String,
    },

    /// Forget the sign-in of an MCP server.
    Logout {
        /// The server's name in `mcp_servers:`.
        server: String,
    },
}

/// Runs a `sven mcp` subcommand.
pub async fn run_mcp_command(cmd: &McpCommands) -> anyhow::Result<()> {
    match cmd {
        McpCommands::Serve {
            tools,
            brave_api_key,
        } => {
            crate::check_requested_tools(tools.as_deref())?;
            let config = sven_bootstrap::Config::load(None)?;
            let root = std::env::current_dir()?;
            let scope = sven_tool_api::PathScope::confined(&root)
                .map_err(|e| anyhow::anyhow!("serving {}: {e}", root.display()))?;
            let registry = std::sync::Arc::new(crate::build_mcp_registry(
                brave_api_key.clone(),
                tools.as_deref(),
                &config.tools,
                scope,
            ));
            crate::serve_stdio(registry).await
        }
        McpCommands::List => list().await,
        McpCommands::Auth { server } => auth(server).await,
        McpCommands::Logout { server } => {
            let manager = manager(false)?;
            manager.sign_out(server).await?;
            println!("Signed out of '{server}'.");
            Ok(())
        }
    }
}

/// A manager over the configured servers. The events it emits are not
/// needed here: each command reports what it did.
fn manager(interactive: bool) -> anyhow::Result<std::sync::Arc<sven_mcp_client::McpManager>> {
    let config = sven_bootstrap::Config::load(None)?;
    let (events, _) = tokio::sync::mpsc::channel(64);
    Ok(sven_mcp_client::McpManager::new(
        config.mcp_servers,
        events,
        interactive,
    ))
}

async fn list() -> anyhow::Result<()> {
    let manager = manager(false)?;
    let names: Vec<String> = manager
        .server_statuses()
        .await
        .into_iter()
        .map(|s| s.name)
        .collect();
    if names.is_empty() {
        println!("No MCP servers are configured (`mcp_servers:` in the configuration file).");
        return Ok(());
    }
    for name in &names {
        let _ = manager.connect(name).await;
    }
    // A server that needs a sign-in reports so from a task of its own.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    for status in manager.server_statuses().await {
        let detail = match &status.status {
            sven_mcp_client::ServerStatus::Connected => {
                format!(
                    "{} tools, {} prompts",
                    status.tool_count, status.prompt_count
                )
            }
            sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
                format!("run `sven mcp auth {}`", status.name)
            }
            sven_mcp_client::ServerStatus::Failed { error } => error.clone(),
            _ => String::new(),
        };
        println!("{:<20} {:<14} {detail}", status.name, status.status.label());
    }
    Ok(())
}

async fn auth(server: &str) -> anyhow::Result<()> {
    let manager = manager(true)?;
    let (pasted_tx, pasted) = tokio::sync::mpsc::channel(4);
    // Lines typed while the sign-in waits are offered to it as the address
    // the browser ended on.
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            if pasted_tx.blocking_send(line).is_err() {
                break;
            }
        }
    });
    let present = |url: &str| {
        eprintln!(
            "Sign in to '{server}' on this page:\n\n  {url}\n\n\
             If the browser cannot come back to this machine, paste the address it \
             ends on here and press Enter."
        );
        sven_mcp_client::open_browser(url);
    };
    manager
        .authenticate_with(server, &present, Some(pasted))
        .await?;
    println!("Signed in to '{server}'.");
    Ok(())
}
