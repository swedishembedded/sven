// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod cli;
mod run;

use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands};

use run::agent_dispatch::run_agent_dispatch_command;
use run::chats::{print_chats, run_migrate_sessions_command};
use run::ci::run_ci;
use run::index::run_index_command;
#[cfg(feature = "memory")]
use run::learn::run_learn_command;
use run::logging::init_logging;
use run::models::{list_models_cmd, list_providers_cmd};
#[cfg(feature = "memory")]
use run::questions::run_questions_command;
use run::task::run_task_command;
// `run_acp_command`/`run_mcp_command` live in `sven-acp`/`sven-mcp`, shared
// verbatim with the standalone binaries.
#[cfg(feature = "network")]
use sven_acp::cli::run_acp_command;
#[cfg(feature = "network")]
use sven_mcp::cli::run_mcp_command;
use run::oauth::run_oauth_callback;
use run::pipeline::{run_map_command, run_reduce_command, run_tee_command};
#[cfg(feature = "network")]
use run::team::{run_as_teammate, run_team_command};
use run::tool::run_tool_command;
#[cfg(feature = "tui")]
use run::tui::run_tui;
use run::workflow::validate_workflow;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install the ring crypto provider for rustls before any TLS code runs.
    // Several crates (reqwest and friends) all pull in rustls 0.23 and
    // without an explicit process-level provider rustls panics.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();

    // In TUI mode writing to stderr corrupts the display.
    // Suppress all tracing output unless the caller explicitly opts in by
    // setting SVEN_LOG_FILE (writes to that file) or by passing --verbose
    // (writes to stderr - only useful with headless / CI mode).
    let is_interactive = !cli.is_headless() && cli.command.is_none();
    #[cfg(feature = "network")]
    let is_server = matches!(
        &cli.command,
        Some(Commands::Mcp { .. }) | Some(Commands::Acp { .. })
    );
    #[cfg(not(feature = "network"))]
    let is_server = false;
    init_logging(cli.verbose, is_interactive, is_server);

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
            Commands::MigrateSessions { dry_run } => {
                return run_migrate_sessions_command(*dry_run);
            }
            Commands::Validate { file } => {
                return validate_workflow(file);
            }
            Commands::AgentDispatch => {
                let config = Arc::new(sven_config::load(cli.config.as_deref())?);
                return run_agent_dispatch_command(config).await;
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
            Commands::Index { command } => {
                return run_index_command(command);
            }
            #[cfg(feature = "memory")]
            Commands::Learn { command } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return run_learn_command(command, &config).await;
            }
            #[cfg(feature = "memory")]
            Commands::Questions { command } => {
                return run_questions_command(command);
            }
            Commands::Task { command } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return run_task_command(command, &config).await;
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
