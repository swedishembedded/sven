// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod cli;
mod run;

use std::sync::Arc;

use clap::Parser;
use cli::{Cli, Commands, DecisionsCommands, DoctorCommands};

use run::agent::run_agent_command;
use run::agent_dispatch::run_agent_dispatch_command;
use run::auth::run_auth_command;
use run::chats::{print_chats, run_migrate_sessions_command};
use run::ci::run_ci;
use run::config::{run_config_command, run_init};
use run::decisions::{run_decisions_models, run_doctor_decisions};
#[cfg(feature = "document")]
use run::document::run_document_command;
use run::harness::run_harness_command;
use run::index::run_index_command;
use run::ledger::run_ledger_command;
use run::logging::init_logging;
use run::models::{list_models_cmd, list_providers_cmd};
use run::provider_doctor::run_doctor_provider;
use run::questions::run_questions_command;
use run::task::run_task_command;
// `run_acp_command`/`run_mcp_command` live in `sven-acp`/`sven-mcp`, shared
// verbatim with the standalone binaries.
use run::oauth::run_oauth_callback;
use run::pipeline::{run_map_command, run_reduce_command, run_tee_command};
#[cfg(all(feature = "tui", feature = "network"))]
use run::remote::attached_agent;
#[cfg(feature = "network")]
use run::remote::{attach_remote_conversation, run_remote_host, show_remote_conversation};
#[cfg(feature = "network")]
use run::team::{run_as_teammate, run_team_command};
use run::tool::run_tool_command;
#[cfg(feature = "tui")]
use run::tui::run_tui;
use run::workflow::validate_workflow;
#[cfg(feature = "network")]
use sven_acp::cli::run_acp_command;
use sven_frontend::Settings;
#[cfg(feature = "network")]
use sven_mcp::cli::run_mcp_command;

fn main() -> anyhow::Result<()> {
    // Install the ring crypto provider for rustls before any TLS code runs.
    // Several crates (reqwest and friends) all pull in rustls 0.23 and
    // without an explicit process-level provider rustls panics.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cli = Cli::parse();
    // A hosted host is confined to its sandbox here, while this process has
    // one thread: the confinement covers only threads and processes started
    // after it, and every runtime thread must be one of them.
    #[cfg(feature = "network")]
    let hosted = run::remote::confine_hosted(&cli)?;
    #[cfg(not(feature = "network"))]
    let hosted = ();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_cli(cli, hosted))
}

#[cfg(feature = "network")]
type Hosted = Option<run::remote::HostedStart>;
#[cfg(not(feature = "network"))]
type Hosted = ();

async fn run_cli(cli: Cli, hosted: Hosted) -> anyhow::Result<()> {
    #[cfg(not(feature = "network"))]
    let () = hosted;
    // Before anything runs: manual approval without a person to ask fails
    // here, never by hanging at the first prompt.
    cli.check_approval()?;

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
    if let Some(path) = &cli.record_requests {
        sven_model_drivers::request_log::install(path)?;
    }

    // Handle subcommands first (before loading config)
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Tool { command } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return run_tool_command(command, &config).await;
            }
            Commands::Doctor {
                command: DoctorCommands::Decisions { json },
            } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return run_doctor_decisions(&config, *json).await;
            }
            Commands::Doctor {
                command: DoctorCommands::Provider { json },
            } => {
                let mut config = Settings::load(cli.config.as_deref())?.runtime;
                if let Some(m) = &cli.model {
                    config.model = config.resolve_model(m);
                }
                return run_doctor_provider(&config, *json).await;
            }
            Commands::Decisions {
                command: DecisionsCommands::Models { json },
            } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return run_decisions_models(&config, *json).await;
            }
            Commands::Agent { command } => {
                let mut config = Settings::load(cli.config.as_deref())?.runtime;
                if let Some(m) = &cli.model {
                    config.model = config.resolve_model(m);
                }
                return run_agent_command(command, config).await;
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
            Commands::Rc(args) => {
                if let Some(hosted) = hosted {
                    return run::remote::run_hosted_host(hosted).await;
                }
                match &args.action {
                    Some(cli::RcAction::Show {
                        conversation,
                        follow,
                    }) => return show_remote_conversation(*conversation, *follow).await,
                    Some(cli::RcAction::Attach {
                        conversation,
                        tui: false,
                    }) => {
                        return attach_remote_conversation(*conversation).await;
                    }
                    #[cfg(feature = "tui")]
                    Some(cli::RcAction::Attach {
                        conversation,
                        tui: true,
                    }) => {
                        let conversation = *conversation;
                        let settings = Settings::load(cli.config.as_deref())?;
                        let task = attached_agent(conversation)?;
                        return run_tui(cli, settings, Some(task)).await;
                    }
                    #[cfg(not(feature = "tui"))]
                    Some(cli::RcAction::Attach { tui: true, .. }) => {
                        anyhow::bail!("this sven was built without the full-screen UI");
                    }
                    None => {}
                }
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return run_remote_host(config, args.working_dir.clone()).await;
            }
            Commands::Completions { shell } => {
                cli::print_completions(*shell);
                return Ok(());
            }
            Commands::ShowConfig => {
                let settings = Settings::load(cli.config.as_deref())?;
                println!("{}", serde_yaml::to_string(&settings).unwrap_or_default());
                return Ok(());
            }
            Commands::OauthCallback { url } => {
                return run_oauth_callback(url).await;
            }
            Commands::Chats { limit } => {
                print_chats(*limit);
                return Ok(());
            }
            Commands::Replay(cli::ReplayArgs {
                file,
                list,
                request,
                repeat,
                seed,
                reported_seed,
                json,
            }) => {
                let mut config = Settings::load(cli.config.as_deref())?.runtime;
                if let Some(m) = &cli.model {
                    config.model = config.resolve_model(m);
                }
                let seed = match (seed, reported_seed) {
                    (Some(s), _) => run::replay::ReplaySeed::Fixed(*s),
                    (None, true) => run::replay::ReplaySeed::Reported,
                    (None, false) => run::replay::ReplaySeed::AsRecorded,
                };
                return run::replay::run_replay_command(
                    file, *list, *request, *repeat, seed, *json, &config,
                )
                .await;
            }
            Commands::Ledger { file, json } => {
                return run_ledger_command(file, *json);
            }
            Commands::MigrateSessions { dry_run } => {
                return run_migrate_sessions_command(*dry_run);
            }
            Commands::Validate { file } => {
                return validate_workflow(file);
            }
            Commands::AgentDispatch => {
                let config = Arc::new(Settings::load(cli.config.as_deref())?.runtime);
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
            #[cfg(feature = "document")]
            Commands::Document { command } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                let code = run_document_command(command, &config).await?;
                if code != 0 {
                    std::process::exit(code);
                }
                return Ok(());
            }
            Commands::Index { command } => {
                return run_index_command(command);
            }
            Commands::Questions { command } => {
                return run_questions_command(command);
            }
            // Offline by construction: no Settings, no config, no model.
            Commands::Harness { command } => {
                let code = run_harness_command(command).await;
                if code != 0 {
                    std::process::exit(code);
                }
                return Ok(());
            }
            Commands::Task { command } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return run_task_command(command, &config).await;
            }
            Commands::ListModels {
                provider,
                catalog,
                json,
            } => {
                let config = Settings::load(cli.config.as_deref())?.runtime;
                return list_models_cmd(&config, provider.as_deref(), *catalog, *json).await;
            }
            Commands::Auth { command } => {
                return run_auth_command(command).await;
            }
            Commands::Init => {
                return run_init();
            }
            Commands::Config { command } => {
                return run_config_command(command, cli.config.as_deref());
            }
            Commands::ListProviders { verbose, json } => {
                return list_providers_cmd(*verbose, *json);
            }
        }
    }

    // A session with no configuration file at all still starts (a local
    // brain server, a stored key), but the person is told how to make one.
    if cli.config.is_none() {
        let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        if sven_config::layer_files(&here).is_empty() {
            eprintln!(
                "sven: no configuration file found; run `sven init` to write a .sven.yaml here"
            );
        }
    }
    let mut settings = Settings::load(cli.config.as_deref())?;
    cli.disclosure.apply(&mut settings.runtime);
    if let Some(mode) = cli.context_assembly {
        settings.runtime.context.assembly.mode = mode.into();
    }

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
        let teammate = run::team::Teammate {
            name: agent_name,
            team: team_name,
            role,
            model: cli.model.clone(),
            instructions: cli.append_system_prompt.clone(),
            initial_task: cli.prompt.clone(),
        };
        return run_as_teammate(teammate, Arc::new(settings.runtime)).await;
    }

    if cli.is_headless() {
        run_ci(cli, Arc::new(settings.runtime)).await
    } else {
        #[cfg(feature = "tui")]
        {
            run_tui(cli, settings, None).await
        }
        #[cfg(not(feature = "tui"))]
        {
            anyhow::bail!(
                "sven was built without the 'tui' feature; pass --headless, a PROMPT, or -f WORKFLOW.md, or rebuild with --features tui"
            );
        }
    }
}
