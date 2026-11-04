// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven mcp` clap grammar and handler, reusable both by the monolithic
//! `sven` binary's `Commands::Mcp` dispatch and the standalone `sven-mcp`
//! binary.

use anyhow::Context as _;
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

        /// Serve over Streamable HTTP at `http://ADDR/mcp` instead of stdio,
        /// for a host that runs tools in another container than its plan.
        /// Needs --token-file. `shell_gate` is served only when --tools
        /// names it.
        #[arg(long, value_name = "ADDR", requires = "token_file")]
        listen: Option<std::net::SocketAddr>,

        /// The file holding the bearer token an HTTP client must present (at
        /// least 32 characters; readable by its owner alone).
        #[arg(long, value_name = "PATH", requires = "listen")]
        token_file: Option<std::path::PathBuf>,
    },

    /// Add an MCP server to your account, or to a configuration file.
    ///
    ///   `sven mcp add github https://api.githubcopilot.com/mcp/`
    ///   `sven mcp add docs https://mcp.example.com/mcp --local --oauth`
    ///   `sven mcp add github --env GITHUB_TOKEN='${GITHUB_TOKEN}' -- npx -y @modelcontextprotocol/server-github`
    ///
    /// It first says where the server is going. Signed in to sven (`sven
    /// auth login`), a URL alone is added to your account (usable from all
    /// your sessions and computers; the sign-in stays with your account,
    /// never on this machine): sven prints the service's sign-in link, opens
    /// the browser, and waits until the server connects. Otherwise, or with
    /// `--local`, a URL is a Streamable HTTP server and anything else the
    /// command (and arguments) of a stdio server, written to a configuration
    /// file; when it asks for an OAuth sign-in, sven starts it at once,
    /// printing the link, opening the browser and accepting a pasted
    /// address. Servers on your account and servers on this machine are
    /// offered together; a server configured here hides the account's copy.
    /// Write a secret as `${VAR}`, so the file holds the variable's name
    /// rather than the secret.
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
        /// Write it to a configuration file and sign in from this machine,
        /// even when signed in to an account.
        #[arg(long)]
        local: bool,
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
            listen,
            token_file,
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
            match (listen, token_file) {
                (Some(addr), Some(path)) => {
                    let token = read_token(path)?;
                    let listener = tokio::net::TcpListener::bind(addr).await?;
                    eprintln!(
                        "sven mcp: serving {} at http://{}/mcp",
                        root.display(),
                        listener.local_addr()?
                    );
                    crate::serve_http(registry, listener, token).await
                }
                _ => crate::serve_stdio(registry).await,
            }
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
            local,
        } => {
            use sven_bootstrap::connectors::{self, Placement};
            let plain = headers.is_empty() && env.is_empty() && client_id.is_none();
            let placement = connectors::current_placement(target, plain, *local);
            let file = sven_mcp_client::config_edit::config_file(*scope)
                .unwrap_or_else(|_| std::path::PathBuf::from("a configuration file"));
            println!("{}", connectors::adding_notice(name, &placement, &file));
            match placement {
                Placement::OutdatedSignIn { origin } => {
                    anyhow::bail!("{}", connectors::outdated_sign_in_notice(&origin))
                }
                Placement::Account(account) => {
                    let url = connectors::account_target(target)
                        .context("an account takes one web address")?;
                    add_to_account(name, url, &account).await
                }
                Placement::Local => {
                    let client =
                        client_id
                            .as_deref()
                            .map(|id| sven_mcp_client::config_edit::OAuthClient {
                                id,
                                secret: client_secret.as_deref(),
                            });
                    let entry = sven_mcp_client::config_edit::server_entry(
                        target, headers, env, *oauth, client,
                    )?;
                    add(name, entry, *scope).await
                }
            }
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

/// Add the server to the signed-in person's account, show where they
/// connect their account of it, and wait for them to finish.
async fn add_to_account(
    name: &str,
    url: &str,
    account: &sven_bootstrap::connectors::Account,
) -> anyhow::Result<()> {
    use sven_bootstrap::connectors;
    let connection = connectors::connect_through_account(account, name, url).await?;
    println!("{}", connectors::sign_in_notice(&connection));
    sven_mcp_client::open_browser(&connection.sign_in_url);
    println!(
        "\nWaiting for you to finish signing in in the browser... \
         (Ctrl+C to stop; the server stays added to your account)"
    );
    let manager = manager(false)?;
    // Its tools arrive once the person finishes (a sign-in lasts 10 minutes).
    let _ = manager.connect(connectors::SERVER).await;
    let before = manager.tool_count(connectors::SERVER).await;
    let finished = manager
        .reconnect_until_more_tools(
            connectors::SERVER,
            before,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(600),
        )
        .await;
    if finished {
        println!(
            "Connected {name}: its tools reach sven as '{}-*' tools.",
            connectors::SERVER
        );
        Ok(())
    } else {
        anyhow::bail!(
            "{name} is not connected yet: the sign-in did not finish within 10 minutes. \
             It stays added to your account; finish signing in on the link above, \
             then check with `sven mcp list`."
        )
    }
}

async fn add(
    name: &str,
    entry: serde_yaml::Value,
    scope: sven_config::scope::Scope,
) -> anyhow::Result<()> {
    let file = sven_mcp_client::config_edit::add(scope, name, entry)?;
    println!("Added MCP server '{name}' to {}.", file.display());

    // Say at once whether it works; when it needs a sign-in, do it now.
    let manager = manager(false)?;
    let Some((status, detail)) = check(&manager, &[name.to_string()]).await.pop() else {
        return Ok(());
    };
    match status {
        sven_mcp_client::ServerStatus::Connected => println!("Connected {name}: {detail}."),
        sven_mcp_client::ServerStatus::NeedsAuth { .. } => {
            println!("{name} needs a sign-in; starting it now.");
            let tools = sign_in_and_connect(name).await?;
            println!("Connected {name}: {tools} tools.");
        }
        _ => println!("It does not connect yet: {detail}"),
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
    use sven_bootstrap::connectors;
    use sven_tool_api::Tool as _;
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
    let rows = check(&manager, &names).await;
    // Servers configured here and the account's `connectors` are offered
    // together; list them apart so it is clear where each lives.
    let (local, account): (Vec<_>, Vec<_>) = names
        .iter()
        .zip(rows)
        .partition(|(name, _)| name.as_str() != connectors::SERVER);
    if !local.is_empty() {
        println!("On this machine (mcp_servers in your configuration files):");
        for (name, (status, detail)) in local {
            println!("{name:<20} {:<14} {detail}", status.label());
        }
    }
    if let Some((name, (status, detail))) = account.into_iter().next() {
        let origin = connectors::signed_in_origin().unwrap_or_default();
        println!(
            "Your sven account at {origin} ('{}-*' tools):",
            connectors::SERVER
        );
        let tools = manager.tools().await;
        let groups = connectors::account_groups(tools.iter().map(|t| t.name()));
        let from = if groups.is_empty() {
            String::new()
        } else {
            format!("; connected services: {}", groups.join(", "))
        };
        println!("{name:<20} {:<14} {detail}{from}", status.label());
    }
    Ok(())
}

/// Sign in to `server` (printing the link, opening the browser, accepting
/// the address a browser elsewhere ended on), and wait for it to connect;
/// its tool count.
async fn sign_in_and_connect(server: &str) -> anyhow::Result<usize> {
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
    let name = server.to_string();
    let present = move |url: &str| {
        println!(
            "{}\n\nIf the browser cannot come back to this machine, paste the address it \
             ends on here and press Enter.",
            sven_mcp_client::sign_in_link(&name, url)
        );
        sven_mcp_client::open_browser(url);
    };
    manager
        .authenticate_with(server, &present, Some(pasted))
        .await?;
    manager
        .wait_connected(server, std::time::Duration::from_secs(60))
        .await
        .map_err(|e| anyhow::anyhow!("signed in, but {server} did not connect: {e:#}"))
}

async fn auth(server: &str) -> anyhow::Result<()> {
    let tools = sign_in_and_connect(server).await?;
    println!("Signed in to '{server}'. Connected {server}: {tools} tools.");
    Ok(())
}

/// The bearer token in `path`: refused when others may read the file.
fn read_token(path: &std::path::Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| anyhow::anyhow!("--token-file {}: {e}", path.display()))?
            .permissions()
            .mode();
        anyhow::ensure!(
            mode & 0o077 == 0,
            "--token-file {} may be read by others; make it readable by its owner alone (chmod 600)",
            path.display()
        );
    }
    let token = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("--token-file {}: {e}", path.display()))?;
    Ok(token.trim().to_string())
}
