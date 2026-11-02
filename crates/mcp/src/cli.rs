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

    /// Add an MCP server to a configuration file and check it.
    ///
    ///   `sven mcp add docs https://mcp.example.com/mcp --oauth`
    ///   `sven mcp add github --env GITHUB_TOKEN='${GITHUB_TOKEN}' -- npx -y @modelcontextprotocol/server-github`
    ///
    /// A URL is a Streamable HTTP server; anything else is the command (and
    /// arguments) of a stdio server. Write a secret as `${VAR}`, so the file
    /// holds the variable's name rather than the secret.
    Add {
        /// The server's name, which prefixes its tools: letters, digits,
        /// `_` and `-`.
        name: String,
        /// The server's URL, or the stdio command and its arguments; put
        /// a command after `--` when its arguments start with `-`.
        #[arg(required = true, num_args = 1..)]
        target: Vec<String>,
        /// An HTTP header to send, `Name=value` (repeatable).
        #[arg(long = "header", value_name = "NAME=VALUE")]
        headers: Vec<String>,
        /// An environment variable for a stdio server, `NAME=value`
        /// (repeatable).
        #[arg(long = "env", value_name = "NAME=VALUE")]
        env: Vec<String>,
        /// The server signs in with OAuth (`oauth: {}`); scopes are
        /// discovered from the server.
        #[arg(long)]
        oauth: bool,
        /// The client id of an OAuth app of your own, for a server that does
        /// not let sven register itself (GitHub's); implies --oauth.
        #[arg(long, value_name = "ID")]
        client_id: Option<String>,
        /// That app's client secret, kept in the configuration file (which is
        /// readable by its owner alone).
        #[arg(long, value_name = "SECRET", requires = "client_id")]
        client_secret: Option<String>,
        /// Which configuration file: `user` (yours, every project),
        /// `project` (this directory's) or `workspace` (the nearest above).
        #[arg(long, default_value = "user")]
        scope: sven_config::scope::Scope,
    },

    /// Remove an MCP server from a configuration file.
    Remove {
        /// The server's name in `mcp_servers:`.
        name: String,
        /// Which configuration file: `user`, `project` or `workspace`.
        #[arg(long, default_value = "user")]
        scope: sven_config::scope::Scope,
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
        McpCommands::Add {
            name,
            target,
            headers,
            env,
            oauth,
            client_id,
            client_secret,
            scope,
        } => {
            let client = client_id
                .as_deref()
                .map(|id| sven_mcp_client::config_edit::OAuthClient {
                    id,
                    secret: client_secret.as_deref(),
                });
            let entry =
                sven_mcp_client::config_edit::server_entry(target, headers, env, *oauth, client)?;
            add(name, entry, *scope).await
        }
        McpCommands::Remove { name, scope } => remove(name, *scope),
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

async fn add(
    name: &str,
    entry: serde_yaml::Value,
    scope: sven_config::scope::Scope,
) -> anyhow::Result<()> {
    let file = sven_mcp_client::config_edit::add(scope, name, entry)?;
    println!("Added MCP server '{name}' to {}.", file.display());

    // Say at once whether it works, and what it needs if not.
    let manager = manager(false)?;
    if let Some((status, detail)) = check(&manager, &[name.to_string()]).await.pop() {
        match status {
            sven_mcp_client::ServerStatus::Connected => println!("It connects: {detail}."),
            sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
                println!("It needs a sign-in: run `sven mcp auth {name}`.");
            }
            _ => println!("It does not connect yet: {detail}"),
        }
    }
    Ok(())
}

/// Connect each of `names` once and say how it went: its status and a line
/// of detail (its tools, what it needs, or the error).
async fn check(
    manager: &std::sync::Arc<sven_mcp_client::McpManager>,
    names: &[String],
) -> Vec<(sven_mcp_client::ServerStatus, String)> {
    let mut errors = std::collections::HashMap::new();
    for name in names {
        if let Err(e) = manager.connect(name).await {
            errors.insert(name.clone(), format!("{e:#}"));
        }
    }
    // A server that needs a sign-in reports so from a task of its own.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let statuses = manager.server_statuses().await;
    names
        .iter()
        .filter_map(|name| statuses.iter().find(|s| &s.name == name))
        .map(|s| {
            let detail = match &s.status {
                sven_mcp_client::ServerStatus::Connected => {
                    format!("{} tools, {} prompts", s.tool_count, s.prompt_count)
                }
                sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
                    format!("run `sven mcp auth {}`", s.name)
                }
                sven_mcp_client::ServerStatus::Disabled => "disabled in the configuration".into(),
                other => errors
                    .get(&s.name)
                    .cloned()
                    .unwrap_or_else(|| other.label().to_string()),
            };
            (s.status.clone(), detail)
        })
        .collect()
}

fn remove(name: &str, scope: sven_config::scope::Scope) -> anyhow::Result<()> {
    let file = sven_mcp_client::config_edit::remove(scope, name)?;
    println!("Removed MCP server '{name}' from {}.", file.display());
    Ok(())
}

/// A manager over the configured servers. The events it emits are not
/// needed here: each command reports what it did.
fn manager(interactive: bool) -> anyhow::Result<std::sync::Arc<sven_mcp_client::McpManager>> {
    let mut config = sven_bootstrap::Config::load(None)?;
    sven_bootstrap::connectors::add_signed_in(&mut config);
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
        println!(
            "No MCP servers are configured. Add one with `sven mcp add NAME URL` \
             or `sven mcp add NAME -- COMMAND ARGS...`."
        );
        return Ok(());
    }
    for (name, (status, detail)) in names.iter().zip(check(&manager, &names).await) {
        println!("{name:<20} {:<14} {detail}", status.label());
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
