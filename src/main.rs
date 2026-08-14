// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod cli;
mod run;

use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};
use sven_config::AgentMode;

#[cfg(feature = "network")]
use run::acp::run_acp_command;
use run::chats::print_chats;
use run::ci::run_ci;
#[cfg(feature = "network")]
use run::cloud::run_cloud_command;
use run::index::run_index_command;
use run::logging::init_logging;
#[cfg(feature = "network")]
use run::mcp::run_mcp_command;
use run::models::{list_models_cmd, list_providers_cmd};
#[cfg(feature = "network")]
use run::node::run_node_command;
use run::oauth::run_oauth_callback;
#[cfg(feature = "network")]
use run::peer::run_peer_command;
use run::pipeline::{run_map_command, run_reduce_command, run_tee_command};
#[cfg(feature = "network")]
use run::share::run_share_command;
#[cfg(feature = "network")]
use run::team::{run_as_teammate, run_team_command};
use run::tool::run_tool_command;
#[cfg(feature = "tui")]
use run::tui::run_tui;
use run::workflow::validate_workflow;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install the ring crypto provider for rustls before any TLS code runs.
    // Multiple crates (reqwest, axum-server, libp2p) all pull in rustls 0.23
    // and without an explicit process-level provider rustls panics.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Backward-compat: SVEN_GATEWAY_TOKEN was the old name for SVEN_NODE_TOKEN.
    // If only the old name is set, copy it so clap env-binding picks it up.
    if std::env::var("SVEN_NODE_TOKEN").is_err() {
        if let Ok(tok) = std::env::var("SVEN_GATEWAY_TOKEN") {
            unsafe { std::env::set_var("SVEN_NODE_TOKEN", tok) };
        }
    }

    let cli = Cli::parse();

    // In TUI mode writing to stderr corrupts the display.
    // Suppress all tracing output unless the caller explicitly opts in by
    // setting SVEN_LOG_FILE (writes to that file) or by passing --verbose
    // (writes to stderr - only useful with headless / CI mode).
    let is_interactive = !cli.is_headless() && cli.command.is_none();
    #[cfg(feature = "network")]
    let is_node = matches!(
        &cli.command,
        Some(Commands::Node { .. })
            | Some(Commands::Mcp { .. })
            | Some(Commands::Acp { .. })
            | Some(Commands::Peer { .. })
            | Some(Commands::Cloud { .. })
            | Some(Commands::Share { .. })
    );
    #[cfg(not(feature = "network"))]
    let is_node = false;
    init_logging(cli.verbose, is_interactive, is_node);

    // Handle subcommands first (before loading config)
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Tool { command } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return run_tool_command(command, &config).await;
            }
            #[cfg(feature = "network")]
            Commands::Mcp { command } => {
                return run_mcp_command(command).await;
            }
            #[cfg(feature = "network")]
            Commands::Acp { command } => {
                return run_acp_command(command).await;
            }
            #[cfg(feature = "network")]
            Commands::Node { command } => {
                return run_node_command(command).await;
            }
            #[cfg(feature = "network")]
            Commands::Peer { command } => {
                return run_peer_command(command).await;
            }
            Commands::Completions { shell } => {
                cli::print_completions(*shell);
                return Ok(());
            }
            Commands::ShowConfig => {
                let config = sven_config::load(cli.config.as_deref())?;
                println!("{}", serde_yaml::to_string(&config).unwrap_or_default());
                return Ok(());
            }
            Commands::OauthCallback { url } => {
                return run_oauth_callback(url).await;
            }
            Commands::Chats { limit } => {
                print_chats(*limit);
                return Ok(());
            }
            Commands::Validate { file } => {
                return validate_workflow(file);
            }
            Commands::Map {
                template,
                concurrency,
                model,
                output_format,
                separator,
            } => {
                return run_map_command(
                    template,
                    *concurrency,
                    model.as_deref(),
                    output_format,
                    separator.as_deref(),
                )
                .await;
            }
            Commands::Tee {
                commands,
                shell,
                separator,
            } => {
                return run_tee_command(commands, shell, separator.as_deref()).await;
            }
            Commands::Reduce {
                prompt,
                model,
                output_format,
                preamble,
            } => {
                return run_reduce_command(
                    prompt,
                    model.as_deref(),
                    output_format,
                    preamble.as_deref(),
                )
                .await;
            }
            #[cfg(feature = "network")]
            Commands::Team { command } => {
                return run_team_command(command);
            }
            #[cfg(feature = "network")]
            Commands::Cloud { command } => {
                return run_cloud_command(command, cli.config.as_deref()).await;
            }
            #[cfg(feature = "network")]
            Commands::Connect {
                uri,
                identity,
                message,
            } => {
                if let Some(message) = message {
                    // One-shot / scriptable: pair, drive one turn, exit.
                    return sven_node::connect::connect_and_run(
                        uri,
                        identity.as_deref(),
                        Some(message),
                    )
                    .await;
                }
                // Interactive: pair over P2P, stand up a loopback WS bridge, and
                // hand off to the SAME node-proxy TUI (`run_tui`) the cloud/node
                // paths use — the full sven interface, over the pairing.
                #[cfg(feature = "tui")]
                {
                    let client =
                        sven_node::connect::pair_client(uri, identity.as_deref()).await?;
                    let bridge = sven_node::connect_bridge::serve_bridge(client).await?;
                    // SAFETY: single-threaded startup, before the TUI/tokio work.
                    unsafe {
                        std::env::set_var("SVEN_NODE_URL", &bridge.ws_url);
                        // The bridge listens on loopback, which any local process
                        // can reach: authenticate with its per-launch token.
                        std::env::set_var("SVEN_NODE_TOKEN", &bridge.token);
                        std::env::set_var("SVEN_NODE_INSECURE", "1");
                    }
                    let tui_cli = Cli::parse_from(["sven"]);
                    let config = Arc::new(sven_config::load(cli.config.as_deref())?);
                    return run_tui(tui_cli, config).await;
                }
                #[cfg(not(feature = "tui"))]
                {
                    anyhow::bail!(
                        "interactive `sven connect` requires the 'tui' feature; pass --message for a non-interactive one-shot connection, or rebuild with --features tui"
                    );
                }
            }
            #[cfg(feature = "network")]
            Commands::Share {
                url,
                token,
                tenant_id,
                share_id,
                title,
                mode,
                ca_cert,
                insecure,
            } => {
                return run_share_command(
                    cli.config.as_deref(),
                    url,
                    token,
                    tenant_id,
                    share_id.as_deref(),
                    title,
                    mode,
                    ca_cert.as_deref(),
                    *insecure,
                )
                .await;
            }
            Commands::Index { command } => {
                return run_index_command(command);
            }
            Commands::ListModels {
                provider,
                refresh,
                json,
            } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return list_models_cmd(&config, provider.as_deref(), *refresh, *json).await;
            }
            Commands::ListProviders { verbose, json } => {
                return list_providers_cmd(*verbose, *json);
            }
        }
    }

    let config = Arc::new(sven_config::load(cli.config.as_deref())?);

    // ── Teammate mode ─────────────────────────────────────────────────────────
    // When --team-name is set (injected by spawn_teammate), skip the normal CI
    // runner and enter the team-member polling loop instead.
    #[cfg(feature = "network")]
    if let Some(team_name) = cli.team_name.clone() {
        let agent_name = cli
            .teammate_name
            .clone()
            .unwrap_or_else(|| "teammate".to_string());
        let role = cli
            .team_role
            .clone()
            .unwrap_or_else(|| "teammate".to_string());
        return run_as_teammate(agent_name, team_name, role, config).await;
    }

    // ── HSM mode resolution ─────────────────────────────────────────────────
    // `SVEN_MODE` env (or `--mode` flag mapped into kernel vocabulary) selects
    // the machine from `sven_machines::ModeRegistry`.  This string is forwarded to
    // `RuntimeBuilder::new(config, mode)` inside run_tui / run_ci / run_gui as
    // those functions are migrated to the kernel path.
    //
    // Priority: SVEN_MODE env > --mode CLI flag > "chat"
    let _hsm_mode: String = std::env::var("SVEN_MODE").unwrap_or_else(|_| {
        match cli.mode {
            AgentMode::Agent => "chat",
            AgentMode::Plan => "sdlc",
            AgentMode::Research => "chat",
            AgentMode::Chat => "chat",
            AgentMode::Sdlc => "sdlc",
        }
        .to_string()
    });

    if cli.is_headless() {
        run_ci(cli, config).await
    } else {
        #[cfg(feature = "tui")]
        {
            run_tui(cli, config).await
        }
        #[cfg(not(feature = "tui"))]
        {
            anyhow::bail!(
                "sven was built without the 'tui' feature; pass --headless, a PROMPT, or -f WORKFLOW.md, or rebuild with --features tui"
            );
        }
    }
}
