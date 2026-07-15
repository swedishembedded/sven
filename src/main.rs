// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
mod cli;

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

use clap::Parser;
use cli::{
    AcpCommands, Cli, CloudCommands, CloudRoleArg, CloudSessionCommands, CloudTenantCommands,
    CloudTlsArg, CloudTokenCommands, Commands, IndexCommands, McpCommands, NodeCommands,
    OutputFormatArg, PeerCommands, TeamCommands, ToolCommands, WebDevicesCommands,
};
use sven_bootstrap::build_cli_tool_registry;
use sven_ci::{find_project_root, CiOptions, CiRunner, OutputFormat};
use sven_ci::{MapOptions, ReduceOptions, TeeOptions};
use sven_config::AgentMode;
use sven_gui::bridge::{SvenApp, SvenAppOptions};
use sven_input::{history, parse_frontmatter, parse_workflow};
use sven_model::catalog::ModelCatalogEntry;
use sven_tui::{App, AppOptions, ModelDirective, NodeBackend, QueuedMessage};

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

    // In TUI/GUI mode writing to stderr corrupts the display.
    // Suppress all tracing output unless the caller explicitly opts in by
    // setting SVEN_LOG_FILE (writes to that file) or by passing --verbose
    // (writes to stderr - only useful with headless / CI mode).
    let is_interactive = !cli.is_headless() && cli.command.is_none();
    let is_tui = is_interactive && !cli.gui;
    let is_gui = is_interactive && cli.gui;
    let is_node = matches!(
        &cli.command,
        Some(Commands::Node { .. })
            | Some(Commands::Mcp { .. })
            | Some(Commands::Acp { .. })
            | Some(Commands::Peer { .. })
            | Some(Commands::Cloud { .. })
    );
    init_logging(cli.verbose, is_tui || is_gui, is_node);

    // Handle subcommands first (before loading config)
    if let Some(cmd) = &cli.command {
        match cmd {
            Commands::Tool { command } => {
                let config = sven_config::load(cli.config.as_deref())?;
                return run_tool_command(command, &config).await;
            }
            Commands::Mcp { command } => {
                return run_mcp_command(command).await;
            }
            Commands::Acp { command } => {
                return run_acp_command(command).await;
            }
            Commands::Node { command } => {
                return run_node_command(command).await;
            }
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
            Commands::Team { command } => {
                return run_team_command(command);
            }
            Commands::Cloud { command } => {
                return run_cloud_command(command, cli.config.as_deref()).await;
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
    // the machine from `sven_core::ModeRegistry`.  This string is forwarded to
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

    if cli.gui {
        run_gui(cli, config).await
    } else if cli.is_headless() {
        run_ci(cli, config).await
    } else {
        run_tui(cli, config).await
    }
}

// ── Tool command handler ──────────────────────────────────────────────────────

async fn run_tool_command(cmd: &ToolCommands, cfg: &sven_config::Config) -> anyhow::Result<()> {
    use sven_tools::tool::ToolCall;

    match cmd {
        // ── sven tool list ────────────────────────────────────────────────────
        ToolCommands::List => {
            let reg = build_cli_tool_registry(cfg);
            let mut schemas = reg.schemas();
            schemas.sort_by(|a, b| a.name.cmp(&b.name));

            let name_width = schemas.iter().map(|s| s.name.len()).max().unwrap_or(0) + 2;
            println!("Built-in tools ({} total):\n", schemas.len());
            for s in &schemas {
                // Grab the first sentence of the description for the summary line.
                let summary = s
                    .description
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                println!("  {:<width$}  {}", s.name, summary, width = name_width);
            }
            println!("\nRun 'sven tool call <TOOL> --help' to see a tool's parameters.");
            Ok(())
        }

        // ── sven tool call [<TOOL> [key=value ...]] ───────────────────────────
        ToolCommands::Call { args } => {
            // Parse the flat args vector:
            //   []                  → show all tools + schemas
            //   ["--help"|"-h"]     → show all tools + schemas
            //   ["<TOOL>"]          → show that tool's schema
            //   ["<TOOL>","--help"] → show that tool's schema
            //   ["<TOOL>", ...]     → execute

            let reg = build_cli_tool_registry(cfg);

            // Strip leading --help / -h flags to determine whether a tool name
            // was provided.
            let stripped: Vec<&String> = args
                .iter()
                .filter(|a| *a != "--help" && *a != "-h")
                .collect();

            if stripped.is_empty() {
                // No tool name → print all tools with their full schemas.
                print_all_tools_help(&reg);
                return Ok(());
            }

            let tool_name = stripped[0];
            let tool_schema = match reg.schemas().into_iter().find(|s| &s.name == tool_name) {
                Some(s) => s,
                None => {
                    let mut available = reg.names();
                    available.sort();
                    eprintln!("error: unknown tool '{tool_name}'");
                    eprintln!("\nAvailable tools (use 'sven tool list' for details):");
                    for name in &available {
                        eprintln!("  {name}");
                    }
                    std::process::exit(2);
                }
            };

            // Remaining args after the tool name (still including any --help).
            let rest: Vec<&String> = args[1..].iter().collect();

            // No params, or only --help / -h → show tool schema.
            let only_help = rest.iter().all(|a| *a == "--help" || *a == "-h");
            if rest.is_empty() || only_help {
                print_tool_help(
                    &tool_schema.name,
                    &tool_schema.description,
                    &tool_schema.parameters,
                );
                return Ok(());
            }

            // ── Build args JSON ───────────────────────────────────────────────
            let json_flag_pos = rest.iter().position(|a| *a == "--json");
            let args_value: serde_json::Value = if let Some(pos) = json_flag_pos {
                let raw = rest
                    .get(pos + 1)
                    .with_context(|| "--json requires a JSON string argument")?;
                serde_json::from_str(raw)
                    .with_context(|| format!("--json value is not valid JSON: {raw}"))?
            } else {
                let kv: Vec<String> = rest
                    .iter()
                    .filter(|a| a.as_str() != "--help" && a.as_str() != "-h")
                    .map(|s| (*s).clone())
                    .collect();
                parse_kv_args(&kv)?
            };

            // ── Execute ───────────────────────────────────────────────────────
            let call = ToolCall {
                id: "cli".to_string(),
                name: tool_name.clone(),
                args: args_value,
            };

            let output = reg.execute(&call).await;

            if output.is_error {
                eprintln!("error: {}", output.content);
                std::process::exit(1);
            } else {
                println!("{}", output.content);
            }
            Ok(())
        }
    }
}

/// Parse `key=value` strings into a `serde_json::Value::Object`.
///
/// Value type inference:
/// - `"true"` / `"false"` → `bool`
/// - All-digit strings → `i64`
/// - Everything else → `String`
fn parse_kv_args(params: &[String]) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for param in params {
        let (key, val_str) = param
            .split_once('=')
            .with_context(|| format!("argument '{param}' is not in key=value form"))?;
        let val = match val_str {
            "true" => serde_json::Value::Bool(true),
            "false" => serde_json::Value::Bool(false),
            s if s.parse::<i64>().is_ok() => {
                serde_json::Value::Number(s.parse::<i64>().unwrap().into())
            }
            s => serde_json::Value::String(s.to_string()),
        };
        map.insert(key.to_string(), val);
    }
    Ok(serde_json::Value::Object(map))
}

/// Print every tool's name + full parameter schema.
///
/// This is what `sven tool call --help` (and `sven tool call` with no args)
/// displays: a complete reference for all available tools.
fn print_all_tools_help(reg: &sven_tools::ToolRegistry) {
    let mut schemas = reg.schemas();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    println!("sven built-in tools ({} total)\n", schemas.len());
    println!(
        "Usage:\n  sven tool call <TOOL> [key=value ...]   - execute a tool\n  \
         sven tool call <TOOL>                    - show that tool's schema\n  \
         sven tool call <TOOL> --json '{{...}}'   - pass raw JSON args\n  \
         sven tool list                           - compact name+description list\n"
    );
    println!("{}\n", "─".repeat(72));
    for schema in &schemas {
        print_tool_help(&schema.name, &schema.description, &schema.parameters);
        println!("{}\n", "─".repeat(72));
    }
}

/// Print a human-readable parameter schema for one tool.
fn print_tool_help(name: &str, description: &str, schema: &serde_json::Value) {
    println!("Tool: {name}\n");
    println!("{description}\n");

    let Some(props) = schema.get("properties").and_then(|p| p.as_object()) else {
        println!("(no parameters)");
        return;
    };

    // Collect required fields.
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let name_width = props.keys().map(|k| k.len()).max().unwrap_or(0) + 2;
    println!("Parameters:");
    for (key, prop) in props {
        let typ = prop
            .get("type")
            .and_then(|t| t.as_str())
            .or_else(|| prop.get("enum").map(|_| "enum"))
            .unwrap_or("any");
        let desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        let req_marker = if required.contains(key.as_str()) {
            "*"
        } else {
            " "
        };

        // Show enum values if present.
        let enum_hint = if let Some(vals) = prop.get("enum").and_then(|v| v.as_array()) {
            let options: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
            format!(" [{}]", options.join(" | "))
        } else {
            String::new()
        };

        println!(
            "  {req_marker} {:<width$}  ({typ}{enum_hint})  {desc}",
            key,
            width = name_width
        );
    }
    println!("\n  * = required");
    println!("\nUsage:");
    println!("  sven tool call {name} key=value ...");
    println!("  sven tool call {name} --json '{{\"key\": \"value\"}}'");
}

// ── OAuth callback handler ───────────────────────────────────────────────────

/// Forward sven:// OAuth callback to the local server.
///
/// The OS protocol handler invokes `sven oauth-callback "sven://sven.mcp/callback?code=X&state=Y"`.
/// We extract the query string and GET http://127.0.0.1:PORT/callback?...
async fn run_oauth_callback(url: &str) -> anyhow::Result<()> {
    let port = std::env::var("SVEN_OAUTH_CALLBACK_PORT").unwrap_or_else(|_| "5598".to_string());
    let query = url.split_once('?').map(|x| x.1).unwrap_or("");
    let callback_url = if query.is_empty() {
        format!("http://127.0.0.1:{port}/callback")
    } else {
        format!("http://127.0.0.1:{port}/callback?{query}")
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("build HTTP client for OAuth callback")?;
    let resp = client
        .get(&callback_url)
        .send()
        .await
        .context("forward OAuth callback to local server")?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "OAuth callback server returned {} (is sven waiting for the callback?)",
            resp.status()
        );
    }
    Ok(())
}

// ── Node command handler ──────────────────────────────────────────────────────

async fn run_node_command(cmd: &NodeCommands) -> anyhow::Result<()> {
    match cmd {
        NodeCommands::Start {
            config: config_path,
            model: model_override,
            provider: provider_override,
            pty_insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let mut sven_config = sven_config::load(None)?;

            // Apply CLI model/provider overrides on top of whatever was loaded
            // from the config file.  Precedence: CLI flag > env var (SVEN_MODEL
            // is already wired through clap) > config file > auto-detected.
            if let Some(ref name) = model_override {
                sven_config.model = sven_model::resolve_model_from_config(&sven_config, name);
            }
            if let Some(ref prov) = provider_override {
                sven_config.model.provider = prov.clone();
            }

            let sven_config = Arc::new(sven_config);
            if let Err(e) = sven_node::node::run(node_config, sven_config, *pty_insecure).await {
                tracing::error!("{e:#}");
                std::process::exit(1);
            }
            Ok(())
        }

        NodeCommands::Authorize {
            uri,
            label,
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::pair_peer(&node_config, uri, label.clone()).await
        }

        NodeCommands::Revoke {
            peer_id,
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::revoke_peer(&node_config, peer_id).await
        }

        NodeCommands::RegenerateToken {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::node::regenerate_token(&node_config)
        }

        NodeCommands::ShowConfig {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            println!(
                "{}",
                serde_yaml::to_string(&node_config).unwrap_or_default()
            );
            Ok(())
        }

        NodeCommands::ListOperators {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::list_peers(&node_config)
        }

        NodeCommands::Exec {
            task,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::exec_task(&node_config, url, token, task, *insecure).await
        }

        NodeCommands::WebDevices { command } => run_web_devices_command(command).await,

        NodeCommands::InstallCa {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let cert_dir = node_config
                .http
                .tls_cert_dir
                .clone()
                .unwrap_or_else(sven_node::tls_default_cert_dir);
            match sven_node::export_ca_cert(&cert_dir)? {
                None => {
                    eprintln!(
                        "No local CA found in {}.\n\
                         Start the node once with tls_mode: local-ca (or auto) to generate it.",
                        cert_dir.display()
                    );
                    std::process::exit(1);
                }
                Some(_) => {
                    sven_node::print_install_instructions(&cert_dir.join("ca-cert.pem"));
                    Ok(())
                }
            }
        }

        NodeCommands::ExportCa {
            config: config_path,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            let cert_dir = node_config
                .http
                .tls_cert_dir
                .clone()
                .unwrap_or_else(sven_node::tls_default_cert_dir);
            match sven_node::export_ca_cert(&cert_dir)? {
                None => {
                    eprintln!(
                        "No local CA found in {}.\n\
                         Start the node once with tls_mode: local-ca (or auto) to generate it.",
                        cert_dir.display()
                    );
                    std::process::exit(1);
                }
                Some(pem) => {
                    print!("{pem}");
                    Ok(())
                }
            }
        }
    }
}

async fn run_web_devices_command(cmd: &WebDevicesCommands) -> anyhow::Result<()> {
    match cmd {
        WebDevicesCommands::List {
            filter,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_list(&node_config, url, token, filter, *insecure).await
        }
        WebDevicesCommands::Approve {
            device_id,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_approve(&node_config, url, token, device_id, *insecure).await
        }
        WebDevicesCommands::Revoke {
            device_id,
            token,
            url,
            config: config_path,
            insecure,
        } => {
            let node_config = sven_node::config::load(config_path.as_deref())?;
            sven_node::web_devices_revoke(&node_config, url, token, device_id, *insecure).await
        }
    }
}

// ── Peer command handler ──────────────────────────────────────────────────────

async fn run_peer_command(cmd: &PeerCommands) -> anyhow::Result<()> {
    match cmd {
        PeerCommands::List {
            config: config_path,
            timeout,
        } => {
            let config = sven_node::config::load(config_path.as_deref())?;
            sven_node::list_agent_peers(&config, *timeout).await
        }

        PeerCommands::Chat {
            peer,
            config: config_path,
        } => {
            let config = sven_node::config::load(config_path.as_deref())?;
            sven_node::peer_chat(&config, peer).await
        }

        PeerCommands::Search {
            peer,
            pattern,
            limit,
        } => sven_node::peer_search(peer.as_deref(), pattern, *limit),
    }
}

// ── MCP command handler ───────────────────────────────────────────────────────

async fn run_acp_command(cmd: &AcpCommands) -> anyhow::Result<()> {
    match cmd {
        AcpCommands::Serve {
            node_url,
            token,
            model,
            provider,
            node_ca,
            insecure_tls,
        } => {
            if let Some(url) = node_url {
                let tok = token.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--token (or SVEN_NODE_TOKEN) is required when --node-url is set"
                    )
                })?;
                let options = sven_node_client::ConnectOptions {
                    extra_ca_pem: node_ca.clone(),
                    insecure_dev: *insecure_tls,
                };
                sven_acp::serve_stdio_node_proxy_with_options(url.clone(), tok, options).await
            } else {
                let mut config = sven_config::load(None)?;
                if let Some(ref name) = model {
                    config.model = sven_model::resolve_model_from_config(&config, name);
                }
                if let Some(ref prov) = provider {
                    config.model.provider = prov.clone();
                }
                sven_acp::serve_stdio(std::sync::Arc::new(config)).await
            }
        }
    }
}

async fn run_mcp_command(cmd: &McpCommands) -> anyhow::Result<()> {
    match cmd {
        McpCommands::Serve {
            tools,
            brave_api_key,
            node_url,
            token,
            node_ca,
            insecure_tls,
        } => {
            if let Some(url) = node_url {
                let tok = token.clone().ok_or_else(|| {
                    anyhow::anyhow!(
                        "--token (or SVEN_NODE_TOKEN) is required when --node-url is set"
                    )
                })?;
                let options = sven_node_client::ConnectOptions {
                    extra_ca_pem: node_ca.clone(),
                    insecure_dev: *insecure_tls,
                };
                sven_mcp::serve_stdio_node_proxy_with_options(url.clone(), tok, options).await
            } else {
                let registry = std::sync::Arc::new(sven_mcp::build_mcp_registry(
                    brave_api_key.clone(),
                    tools.as_deref(),
                ));
                sven_mcp::serve_stdio(registry).await
            }
        }
    }
}

/// Validate a workflow file: parse frontmatter, count steps, report to stdout.
fn validate_workflow(file: &std::path::Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(file)
        .with_context(|| format!("reading workflow file {}", file.display()))?;

    let (frontmatter, markdown_body) = parse_frontmatter(&content);

    let workflow = parse_workflow(markdown_body);

    // Title: frontmatter overrides H1
    let title = frontmatter
        .as_ref()
        .and_then(|fm| fm.title.as_deref())
        .or(workflow.title.as_deref());
    if let Some(t) = title {
        println!("Title: {t}");
    }

    if let Some(fm) = &frontmatter {
        println!("Frontmatter: OK");
        if let Some(models) = &fm.models {
            println!("  models ({}):", models.len());
            let mut pairs: Vec<_> = models.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (mode, model) in pairs {
                println!("    {mode}: {model}");
            }
        }
        if let Some(vars) = &fm.vars {
            println!("  vars ({}):", vars.len());
            let mut pairs: Vec<_> = vars.iter().collect();
            pairs.sort_by_key(|(k, _)| k.as_str());
            for (k, v) in pairs {
                println!("    {k} = {v}");
            }
        }
    } else {
        println!("Frontmatter: (none)");
    }

    if let Some(preamble) = &workflow.system_prompt_append {
        println!(
            "Preamble: {} chars (appended to system prompt)",
            preamble.chars().count()
        );
    }

    let mut queue = workflow.steps;
    let total = queue.len();
    println!("Steps: {total}");

    let mut i = 0;
    while let Some(step) = queue.pop() {
        i += 1;
        let label = step.label.as_deref().unwrap_or("(unlabelled)");
        let mode = step.options.mode.as_deref().unwrap_or("(inherit)");
        let provider = step.options.provider.as_deref().unwrap_or("(inherit)");
        let model = step.options.model.as_deref().unwrap_or("(inherit)");
        let timeout = step
            .options
            .timeout_secs
            .map(|t| format!("{t}s"))
            .unwrap_or_else(|| "(inherit)".to_string());
        println!("  Step {i}/{total}: {label:?}  mode={mode}  provider={provider}  model={model}  timeout={timeout}");
        if !step.content.is_empty() {
            let preview = step.content.chars().take(80).collect::<String>();
            let ellipsis = if step.content.chars().count() > 80 {
                "..."
            } else {
                ""
            };
            println!("    {preview}{ellipsis}");
        }
    }

    println!("\nWorkflow is valid.");
    Ok(())
}

/// List available models, optionally querying the provider API for live data.
async fn list_models_cmd(
    config: &sven_config::Config,
    provider_filter: Option<&str>,
    refresh: bool,
    as_json: bool,
) -> anyhow::Result<()> {
    // Validate provider filter against the registry.
    if let Some(prov) = provider_filter {
        if sven_model::get_driver(prov).is_none() {
            eprintln!("Unknown provider: {prov:?}");
            eprintln!("\nAvailable providers (run `sven list-providers` for details):");
            for d in sven_model::list_drivers() {
                eprintln!("  {:20} {}", d.id, d.name);
            }
            anyhow::bail!("Invalid provider: {prov}");
        }
    }

    let entries: Vec<ModelCatalogEntry> = if refresh {
        // Query the configured (or filtered) provider's live API.
        let model_cfg = if let Some(prov) = provider_filter {
            let mut c = config.model.clone();
            c.provider = prov.to_string();
            c
        } else {
            config.model.clone()
        };
        let model = sven_model::from_config(&model_cfg)?;
        let mut live = model.list_models().await?;
        if let Some(prov) = provider_filter {
            live.retain(|e| e.provider == prov);
        }
        live
    } else {
        // Use static catalog only.
        let mut all = sven_model::catalog::static_catalog();
        if let Some(prov) = provider_filter {
            all.retain(|e| e.provider == prov);
        }
        all.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
        all
    };

    if as_json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No models found.");
        return Ok(());
    }

    // Determine column widths.
    let id_w = entries
        .iter()
        .map(|e| e.id.len())
        .max()
        .unwrap_or(10)
        .max(10);
    let prov_w = entries
        .iter()
        .map(|e| e.provider.len())
        .max()
        .unwrap_or(8)
        .max(8);

    println!(
        "{:<id_w$}  {:<prov_w$}  {:>12}  {:>16}  DESCRIPTION",
        "ID",
        "PROVIDER",
        "CTX WINDOW",
        "MAX OUT TOKENS",
        id_w = id_w,
        prov_w = prov_w,
    );
    println!("{}", "-".repeat(id_w + prov_w + 50));

    for e in &entries {
        let ctx = if e.context_window == 0 {
            "  -".to_string()
        } else {
            format!("{:>12}", e.context_window)
        };
        let max_out = if e.max_output_tokens == 0 {
            "  -".to_string()
        } else {
            format!("{:>16}", e.max_output_tokens)
        };
        println!(
            "{:<id_w$}  {:<prov_w$}  {}  {}  {}",
            e.id,
            e.provider,
            ctx,
            max_out,
            e.description,
            id_w = id_w,
            prov_w = prov_w,
        );
    }
    println!("\nTotal: {} model(s)", entries.len());
    Ok(())
}

/// List all registered model providers.
fn list_providers_cmd(verbose: bool, as_json: bool) -> anyhow::Result<()> {
    let drivers = sven_model::list_drivers();

    if as_json {
        #[derive(serde::Serialize)]
        struct ProviderJson {
            id: &'static str,
            name: &'static str,
            description: &'static str,
            default_api_key_env: Option<&'static str>,
            default_base_url: Option<&'static str>,
            requires_api_key: bool,
        }
        let rows: Vec<ProviderJson> = drivers
            .iter()
            .map(|d| ProviderJson {
                id: d.id,
                name: d.name,
                description: d.description,
                default_api_key_env: d.default_api_key_env,
                default_base_url: d.default_base_url,
                requires_api_key: d.requires_api_key,
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("Supported Model Providers ({} total)\n", drivers.len());

    if verbose {
        for d in drivers {
            println!("  {} - {}", d.id, d.name);
            println!("    {}", d.description);
            if let Some(env) = d.default_api_key_env {
                println!("    API key env : {env}");
            }
            if let Some(url) = d.default_base_url {
                println!("    Default URL : {url}");
            }
            if !d.requires_api_key {
                println!("    Auth        : none required");
            }
            println!();
        }
    } else {
        let id_w = drivers
            .iter()
            .map(|d| d.id.len())
            .max()
            .unwrap_or(10)
            .max(10);
        let name_w = drivers
            .iter()
            .map(|d| d.name.len())
            .max()
            .unwrap_or(8)
            .max(8);
        println!("{:<id_w$}  {:<name_w$}  DESCRIPTION", "ID", "NAME");
        println!("{}", "-".repeat(id_w + name_w + 40));
        for d in drivers {
            println!("{:<id_w$}  {:<name_w$}  {}", d.id, d.name, d.description);
        }
        println!("\nUse `sven list-providers --verbose` for API key and URL details.");
        println!("Use `sven list-models --provider <ID>` to see models for a specific provider.");
    }
    Ok(())
}

// ── Map / Tee / Reduce command handlers ──────────────────────────────────────

/// `sven map TEMPLATE` - run one agent per stdin line.
async fn run_map_command(
    template: &str,
    concurrency: usize,
    model: Option<&str>,
    output_format: &str,
    separator: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = MapOptions {
        template: template.to_string(),
        concurrency,
        model: model.map(|m| m.to_string()),
        sven_bin: None,
        extra_args: Vec::new(),
        output_format: output_format.to_string(),
        section_separator: separator.map(|s| s.to_string()),
    };

    sven_ci::pipe::run_map(opts, stdin_data).await
}

/// `sven tee CMD...` - broadcast stdin to N parallel commands.
async fn run_tee_command(
    commands: &[String],
    shell: &str,
    separator: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = TeeOptions {
        commands: commands.to_vec(),
        shell: Some(shell.to_string()),
        section_separator: separator.map(|s| s.to_string()),
    };

    sven_ci::pipe::run_tee(opts, stdin_data).await
}

/// `sven reduce PROMPT` - aggregate stdin into one synthesis agent.
async fn run_reduce_command(
    prompt: &str,
    model: Option<&str>,
    output_format: &str,
    preamble: Option<&str>,
) -> anyhow::Result<()> {
    let stdin_data = read_stdin_to_string()?;

    let opts = ReduceOptions {
        prompt: prompt.to_string(),
        model: model.map(|m| m.to_string()),
        sven_bin: None,
        output_format: output_format.to_string(),
        preamble: preamble.map(|p| p.to_string()),
    };

    sven_ci::pipe::run_reduce(opts, stdin_data).await
}

/// Read all of stdin into a string.
fn read_stdin_to_string() -> anyhow::Result<String> {
    let mut buf = String::new();
    io::stdin()
        .read_to_string(&mut buf)
        .context("reading stdin")?;
    Ok(buf)
}

// ── Index command handler ─────────────────────────────────────────────────────

fn run_index_command(cmd: &IndexCommands) -> anyhow::Result<()> {
    let project_root =
        sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match cmd {
        IndexCommands::Build { quiet } => sven_ci::index::cmd_build(&project_root, *quiet),
        IndexCommands::Query { query, limit } => {
            sven_ci::index::cmd_query(&project_root, query, *limit)
        }
        IndexCommands::Stats => sven_ci::index::cmd_stats(&project_root),
    }
}

// ── Team command handler ──────────────────────────────────────────────────────

fn run_team_command(cmd: &TeamCommands) -> anyhow::Result<()> {
    match cmd {
        TeamCommands::List => sven_team::cli::cmd_list(),

        TeamCommands::Status { name } => sven_team::cli::cmd_status(name),

        TeamCommands::Create {
            name,
            goal,
            max_active,
            token_budget,
        } => sven_team::cli::cmd_create(name, goal.as_deref(), *max_active, *token_budget),

        TeamCommands::Start {
            file,
            sven_bin,
            dry_run,
        } => sven_team::cli::cmd_start(file, sven_bin.as_deref(), *dry_run),

        TeamCommands::Cleanup { name, force } => sven_team::cli::cmd_cleanup(name, *force),

        TeamCommands::Definitions => {
            let project_root =
                sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
            sven_team::cli::cmd_definitions(&project_root)
        }

        TeamCommands::Init { name, goal } => {
            let project_root =
                sven_ci::find_project_root().unwrap_or_else(|_| std::path::PathBuf::from("."));
            sven_team::cli::cmd_init(&project_root, name, goal.as_deref())
        }

        TeamCommands::Watch {
            name,
            interval,
            timeout,
        } => sven_team::cli::cmd_watch(name, *interval, *timeout),
    }
}

// ── Cloud command handler ─────────────────────────────────────────────────────

impl From<CloudRoleArg> for sven_cloud::Role {
    fn from(role: CloudRoleArg) -> Self {
        match role {
            CloudRoleArg::Companion => sven_cloud::Role::Companion,
            CloudRoleArg::Operator => sven_cloud::Role::Operator,
            CloudRoleArg::ClientViewer => sven_cloud::Role::ClientViewer,
        }
    }
}

/// Derive a stable tenant id (slug) from a human name: lowercase, non-alnum
/// runs collapsed to a single '-', trimmed. Empty input falls back to "tenant".
fn tenant_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "tenant".to_string()
    } else {
        slug
    }
}

/// Open (creating if needed) the SQLite control-plane store at `db`.
fn open_cloud_store(db: &std::path::Path) -> anyhow::Result<Arc<dyn sven_cloud::CloudStore>> {
    let store = sven_cloud::SqliteStore::open(db)
        .with_context(|| format!("opening control-plane database {}", db.display()))?;
    Ok(Arc::new(store))
}

async fn run_cloud_command(
    cmd: &CloudCommands,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    match cmd {
        CloudCommands::Serve {
            db,
            bind,
            tls,
            cert,
            key,
            ca_out,
        } => {
            run_cloud_serve(
                db,
                bind,
                *tls,
                cert.as_deref(),
                key.as_deref(),
                ca_out.as_deref(),
                config_path,
            )
            .await
        }
        CloudCommands::Tenant { command } => run_cloud_tenant_command(command),
        CloudCommands::Token { command } => run_cloud_token_command(command),
        CloudCommands::Session { command } => run_cloud_session_command(command).await,
        CloudCommands::DemoSeed {
            tenant_name,
            db,
            url,
            credit_micro_usd,
            fee_micro_usd,
            ttl,
            out_dir,
        } => run_cloud_demo_seed(
            tenant_name,
            db,
            url,
            *credit_micro_usd,
            *fee_micro_usd,
            ttl,
            out_dir.as_deref(),
        ),
    }
}

/// Provision a turnkey demo tenant: subscribe + fund it and mint an operator
/// and a companion token, mirroring how the cloud-session runtime test funds a
/// tenant. Writes both secrets to `<out>/operator.token` and
/// `<out>/companion.token` and prints the run commands.
fn run_cloud_demo_seed(
    tenant_name: &str,
    db: &std::path::Path,
    url: &str,
    credit_micro_usd: i64,
    fee_micro_usd: i64,
    ttl: &str,
    out_dir: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use sven_cloud::{IdentityService, Role, SessionGate, TenantRecord, UsageMeter};
    use sven_metering::{CreditLedger, PricingCatalog};

    let store = open_cloud_store(db)?;
    let tenant_id = tenant_slug(tenant_name);

    // ── Tenant (idempotent) ──────────────────────────────────────────────────
    if store.tenant(&tenant_id)?.is_none() {
        store
            .create_tenant(&TenantRecord {
                id: tenant_id.clone(),
                name: tenant_name.to_string(),
                created_at: chrono::Utc::now().timestamp(),
            })
            .with_context(|| format!("creating tenant {tenant_id:?}"))?;
        println!("created tenant {tenant_id} ({tenant_name})");
    } else {
        println!("tenant {tenant_id} already exists; reusing");
    }

    // ── Subscription + credit on the SAME ledger `serve` reads ────────────────
    let ledger_path = db.with_file_name("credit.jsonl");
    let meter = UsageMeter::new(PricingCatalog::builtin(), CreditLedger::new(&ledger_path));
    let now = chrono::Utc::now();
    let period = SessionGate::period_of(now);
    // Booking the platform fee for the current period activates the
    // subscription (idempotent per tenant+period).
    meter
        .ledger()
        .record_platform_fee(&tenant_id, &period, fee_micro_usd)
        .with_context(|| format!("booking platform fee for {tenant_id} {period}"))?;
    // Top up only when the balance would not otherwise pass the gate, so
    // re-running the seed does not endlessly inflate the balance.
    let balance_before = meter.balance(&tenant_id)?.balance_micro_usd();
    if balance_before <= 0 {
        meter
            .ledger()
            .record_credit(&tenant_id, credit_micro_usd, "demo-seed")
            .with_context(|| format!("granting demo credit to {tenant_id}"))?;
    }
    let balance = meter.balance(&tenant_id)?.balance_micro_usd();
    println!("  subscription: active for {period} (fee {fee_micro_usd} micro-USD)");
    println!("  balance     : {balance} micro-USD");

    // ── Operator + companion tokens ──────────────────────────────────────────
    let identity = IdentityService::new(Arc::clone(&store));
    let ttl = humantime::parse_duration(ttl)
        .with_context(|| format!("parsing --ttl {ttl:?} (e.g. 30d, 12h)"))?;
    let operator = identity
        .mint_token(&tenant_id, Role::Operator, ttl)
        .with_context(|| format!("minting operator token for {tenant_id}"))?;
    let companion = identity
        .mint_token(&tenant_id, Role::Companion, ttl)
        .with_context(|| format!("minting companion token for {tenant_id}"))?;

    // ── Persist the secrets (0600) beside the db (or --out-dir) ──────────────
    let out = out_dir
        .map(std::path::Path::to_path_buf)
        .or_else(|| db.parent().map(std::path::Path::to_path_buf))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&out)
        .with_context(|| format!("creating token output dir {}", out.display()))?;
    let operator_path = out.join("operator.token");
    let companion_path = out.join("companion.token");
    write_secret_file(&operator_path, &operator.secret)?;
    write_secret_file(&companion_path, &companion.secret)?;

    // ── Derive the tether URL from the portal URL (https→wss, +/tether) ──────
    let tether_url = derive_tether_url(url);
    let ca_path = db.with_file_name("cloud-ca.pem");

    println!();
    println!("demo tenant ready — tokens written:");
    println!("  operator  : {}", operator_path.display());
    println!("  companion : {}", companion_path.display());
    println!();
    println!("1) start the companion (remote hands) — jail it to a demo workdir:");
    println!(
        "     sven-companion --url {tether} --ca-cert {ca} \\\n\
         \x20      --tenant-id {tenant} --token-file {ctok} --fs-root <demo-workdir>",
        tether = tether_url,
        ca = ca_path.display(),
        tenant = tenant_id,
        ctok = companion_path.display(),
    );
    println!();
    println!("2) drive a session as the operator:");
    println!(
        "     sven cloud session start --url {url} --ca-cert {ca} \\\n\
         \x20      --token \"$(cat {otok})\" --prompt \"please read the demo report\"",
        url = url,
        ca = ca_path.display(),
        otok = operator_path.display(),
    );
    Ok(())
}

/// Writes `secret` to `path`, owner-read/write only on Unix.
fn write_secret_file(path: &std::path::Path, secret: &str) -> anyhow::Result<()> {
    std::fs::write(path, format!("{secret}\n"))
        .with_context(|| format!("writing token file {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", path.display()))?;
    }
    Ok(())
}

/// Derives the companion tether URL from the portal base URL: `https`→`wss`,
/// `http`→`ws`, then append `/tether`.
fn derive_tether_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let ws = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        trimmed.to_string()
    };
    format!("{ws}/tether")
}

async fn run_cloud_serve(
    db: &std::path::Path,
    bind: &str,
    tls: CloudTlsArg,
    cert: Option<&std::path::Path>,
    key: Option<&std::path::Path>,
    ca_out: Option<&std::path::Path>,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use std::time::Duration;

    use sven_cloud::{
        portal_router, CloudConfig, CloudServer, CloudSessionLauncher, CloudTls, CompanionRegistry,
        IdentityService, PortalConfig, PortalState, SessionFeed, SessionGate, UsageMeter,
    };
    use sven_metering::{CreditLedger, PricingCatalog};

    let store = open_cloud_store(db)?;
    let identity = Arc::new(IdentityService::new(Arc::clone(&store)));
    let registry = Arc::new(CompanionRegistry::new());
    let feed = Arc::new(SessionFeed::new());

    // Metering + session gating: prices every turn and refuses unfunded /
    // unsubscribed sessions. The ledger lives beside the control-plane DB.
    let ledger_path = db.with_file_name("credit.jsonl");
    let meter = Arc::new(UsageMeter::new(
        PricingCatalog::builtin(),
        CreditLedger::new(ledger_path),
    ));
    let gate = Arc::new(SessionGate::new(Arc::clone(&store), Arc::clone(&meter)));

    // The cloud session runtime: `POST /sessions` builds and drives a real
    // kernel (metered LLM in the cloud, tool calls routed to the tenant's
    // companion) from the deployment's model config.
    // Sessions are driven from the deployment's model config (the same `--config`
    // the operator passed to `serve`), so the demo's mock provider — or a real
    // provider in production — actually drives cloud sessions.
    let agent_config = Arc::new(sven_config::load(config_path).unwrap_or_default());
    let launcher = Arc::new(CloudSessionLauncher::new(
        Arc::clone(&registry),
        Arc::clone(&feed),
        Arc::clone(&meter),
        Arc::clone(&store),
        agent_config,
    ));

    // The human portal, served on the same TLS listener as the tether.
    let portal_state = PortalState::new(
        &PortalConfig {
            rp_id: "localhost".to_string(),
            rp_origin: "https://localhost".to_string(),
            rp_name: "sven cloud".to_string(),
            devices_path: db.with_file_name("portal-devices.yaml"),
            session_ttl: Duration::from_secs(24 * 60 * 60),
        },
        Arc::clone(&identity),
    )
    .context("building the portal")?
    .with_session_gate(gate)
    .with_live_feed(Arc::clone(&feed))
    .with_companion_registry(Arc::clone(&registry))
    .with_session_launcher(launcher);
    let portal = portal_router(portal_state);

    let tls_mode = match tls {
        CloudTlsArg::LocalCa | CloudTlsArg::SelfSigned => CloudTls::SelfSigned,
        CloudTlsArg::InsecureDev => CloudTls::InsecureDev,
        CloudTlsArg::Files => {
            let cert = cert
                .context("--tls files requires --cert")?
                .to_path_buf();
            let key = key.context("--tls files requires --key")?.to_path_buf();
            CloudTls::Pem { cert, key }
        }
    };

    let config = CloudConfig::new(bind.to_string()).with_tls(tls_mode);
    let server =
        CloudServer::start_with_router(config, identity, registry, Some(feed), Some(portal))
            .await
            .context("starting the cloud control plane")?;

    println!("sven cloud: tether + portal listening");
    println!("  tether URL : {}", server.tether_url());
    println!("  portal     : same host/port (POST /sessions, GET /events, GET /)");
    if let Some(ca_pem) = server.ca_pem() {
        let ca_path = ca_out
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| db.with_file_name("cloud-ca.pem"));
        std::fs::write(&ca_path, ca_pem)
            .with_context(|| format!("writing CA certificate to {}", ca_path.display()))?;
        println!("  CA cert    : {}", ca_path.display());
        println!(
            "  companions : sven-companion --url {} --ca-cert {} --tenant-id <id> --token <secret>",
            server.tether_url(),
            ca_path.display()
        );
    }
    println!("  database   : {}", db.display());
    println!("Press Ctrl-C to stop.");

    tokio::signal::ctrl_c()
        .await
        .context("waiting for shutdown signal")?;
    println!("sven cloud: shutting down");
    server.shutdown().await;
    Ok(())
}

fn run_cloud_tenant_command(cmd: &CloudTenantCommands) -> anyhow::Result<()> {
    use sven_cloud::TenantRecord;

    match cmd {
        CloudTenantCommands::Create { name, plan, db } => {
            let store = open_cloud_store(db)?;
            let id = tenant_slug(name);
            let record = TenantRecord {
                id: id.clone(),
                name: name.clone(),
                created_at: chrono::Utc::now().timestamp(),
            };
            store
                .create_tenant(&record)
                .with_context(|| format!("creating tenant {id:?}"))?;
            println!("created tenant");
            println!("  id   : {id}");
            println!("  name : {name}");
            if let Some(plan) = plan {
                // The store has no plan column yet; surface it as informational.
                println!("  plan : {plan} (informational; not persisted)");
            }
            Ok(())
        }
        CloudTenantCommands::List { db } => {
            let store = open_cloud_store(db)?;
            let tenants = store.tenants().context("listing tenants")?;
            if tenants.is_empty() {
                println!("no tenants");
                return Ok(());
            }
            println!("{:<24}  {:<28}  CREATED", "ID", "NAME");
            for t in &tenants {
                println!("{:<24}  {:<28}  {}", t.id, t.name, t.created_at);
            }
            Ok(())
        }
    }
}

fn run_cloud_token_command(cmd: &CloudTokenCommands) -> anyhow::Result<()> {
    use sven_cloud::{IdentityService, Role};

    match cmd {
        CloudTokenCommands::Mint {
            tenant,
            role,
            ttl,
            db,
        } => {
            let store = open_cloud_store(db)?;
            let identity = IdentityService::new(store);
            let role: Role = (*role).into();
            let ttl = match ttl {
                Some(spec) => humantime::parse_duration(spec)
                    .with_context(|| format!("parsing --ttl {spec:?} (e.g. 30d, 12h)"))?,
                // Default to the role cap; mint clamps to it regardless.
                None => std::time::Duration::from_secs(
                    u64::try_from(role.max_token_ttl_secs()).unwrap_or(0),
                ),
            };
            let minted = identity
                .mint_token(tenant, role, ttl)
                .with_context(|| format!("minting {role} token for tenant {tenant:?}"))?;
            // Metadata to stderr, the secret alone to stdout — captured with a
            // redirect, never logged, and unrecoverable afterwards.
            eprintln!("minted {role} token for tenant {tenant}");
            eprintln!("  id         : {}", minted.record.id);
            eprintln!("  expires_at : {}", minted.record.expires_at);
            eprintln!("  secret (shown once — store it now):");
            println!("{}", minted.secret);
            Ok(())
        }
        CloudTokenCommands::Revoke { token_id, db } => {
            let store = open_cloud_store(db)?;
            let identity = IdentityService::new(store);
            if identity
                .revoke(token_id)
                .with_context(|| format!("revoking token {token_id:?}"))?
            {
                println!("revoked token {token_id}");
                Ok(())
            } else {
                anyhow::bail!("no such token: {token_id}");
            }
        }
    }
}

async fn run_cloud_session_command(cmd: &CloudSessionCommands) -> anyhow::Result<()> {
    use sven_cloud::{run_operator_session, OperatorSessionOptions};

    match cmd {
        CloudSessionCommands::Start {
            url,
            token,
            prompt,
            mode,
            ca_cert,
            insecure,
        } => {
            run_operator_session(OperatorSessionOptions {
                base_url: url.clone(),
                token: token.clone(),
                prompt: prompt.clone(),
                mode: mode.clone(),
                ca_pem: ca_cert.clone(),
                insecure_dev: *insecure,
            })
            .await
            .with_context(|| format!("running an operator session against {url}"))
        }
    }
}

/// Print the list of saved conversations to stdout.
fn print_chats(limit: usize) {
    match history::list(Some(limit)) {
        Ok(entries) if entries.is_empty() => {
            println!("No saved conversations found.");
            println!(
                "Conversations are stored in: {}",
                history::history_dir().display()
            );
        }
        Ok(entries) => {
            println!(
                "{:<45}  {:<16}  {:<5}  TITLE",
                "ID (use with --resume)", "DATE", "TURNS"
            );
            println!("{}", "-".repeat(95));
            for e in &entries {
                let display_id = if e.id.len() > 44 {
                    format!("{}...", &e.id[..43])
                } else {
                    e.id.clone()
                };
                let date = e.timestamp.replace('T', " ");
                let date = &date[..16.min(date.len())];
                let title = if e.title.chars().count() > 50 {
                    format!("{}...", e.title.chars().take(49).collect::<String>())
                } else {
                    e.title.clone()
                };
                println!(
                    "{:<45}  {:<16}  {:<5}  {}",
                    display_id, date, e.turns, title
                );
            }
            println!("\nTotal: {} conversation(s)", entries.len());
            println!("History dir: {}", history::history_dir().display());
        }
        Err(e) => {
            eprintln!("Error listing conversations: {e}");
            std::process::exit(1);
        }
    }
}

/// Launch `fzf` and let the user pick a conversation to resume.
fn pick_chat_with_fzf() -> anyhow::Result<Option<String>> {
    let entries = history::list(None).context("listing saved conversations")?;
    if entries.is_empty() {
        anyhow::bail!(
            "No saved conversations found.\n\
             Start a conversation with sven first, then use --resume to continue it."
        );
    }

    let lines: String = entries
        .iter()
        .map(|e| {
            let date = e.timestamp.replace('T', " ");
            let date = &date[..16.min(date.len())];
            let turns_label = if e.turns == 1 {
                "1 turn".to_string()
            } else {
                format!("{} turns", e.turns)
            };
            format!("{}\t{}\t{}\t{}", e.id, date, e.title, turns_label)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut child = std::process::Command::new("fzf")
        .args([
            "--delimiter=\t",
            "--with-nth=3,2,4",
            "--tabstop=1",
            "--header=Resume conversation  (Enter: open · Esc: cancel)",
            "--header-first",
            "--height=50%",
            "--min-height=10",
            "--reverse",
            "--no-sort",
            "--bind=ctrl-/:toggle-preview",
            "--preview=echo {}",
            "--preview-window=down:2:wrap:hidden",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context(
            "failed to launch fzf - make sure fzf is installed\n\
             (https://github.com/junegunn/fzf or `apt install fzf`)",
        )?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(lines.as_bytes());
    }

    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Ok(None);
    }

    let selected = String::from_utf8_lossy(&output.stdout);
    let selected = selected.trim();
    if selected.is_empty() {
        return Ok(None);
    }

    let id = selected.split('\t').next().unwrap_or("").trim().to_string();
    if id.is_empty() {
        anyhow::bail!("fzf returned an unexpected selection: {selected:?}");
    }
    Ok(Some(id))
}

// ── Teammate runner ───────────────────────────────────────────────────────────

/// Team-member polling loop.
///
/// Registers the process in the team config, then repeatedly:
///   1. Checks for a shutdown signal (status == Closed in the team config).
///   2. Calls `claim_next(agent_name)` to atomically grab a pending task
///      assigned to this agent (or any unassigned task).
///   3. Runs `CiRunner` with the task description as the prompt.
///   4. Marks the task completed (or failed) with the agent's final response.
///
/// Exits when the shutdown signal is received, the team directory disappears,
/// or a fatal error prevents task store access.
async fn run_as_teammate(
    agent_name: String,
    team_name: String,
    role_str: String,
    config: Arc<sven_config::Config>,
) -> anyhow::Result<()> {
    use sven_ci::{find_project_root, CiOptions, CiRunner, OutputFormat};
    use sven_config::AgentMode;
    use sven_team::{
        config::{MemberStatus, TeamConfigStore, TeamMember, TeamRole},
        task::TaskStore,
    };

    let peer_id = sven_team::teammate_stable_peer_id(&team_name, &agent_name);

    let role = match role_str.as_str() {
        "implementer" => TeamRole::Implementer,
        "reviewer" => TeamRole::Reviewer,
        "explorer" => TeamRole::Explorer,
        "tester" => TeamRole::Tester,
        _ => TeamRole::Teammate,
    };

    // ── Register in team config ───────────────────────────────────────────────
    let cfg_store = TeamConfigStore::open(&team_name)
        .with_context(|| format!("opening team config for '{team_name}'"))?;

    let our_pid = std::process::id();
    let _ = cfg_store.modify(|cfg| {
        // Idempotent: update PID on re-registration; add on first start.
        if let Some(m) = cfg.members.iter_mut().find(|m| m.peer_id == peer_id) {
            m.pid = Some(our_pid);
            m.status = MemberStatus::Active;
        } else {
            cfg.members.push(TeamMember {
                peer_id: peer_id.clone(),
                name: agent_name.clone(),
                role: role.clone(),
                model: None,
                status: MemberStatus::Active,
                current_task_id: None,
                joined_at: chrono::Utc::now(),
                pid: Some(our_pid),
            });
        }
    });

    eprintln!("[teammate:{agent_name}] registered in team '{team_name}' peer_id={peer_id}");

    let task_store = TaskStore::open(&team_name)
        .with_context(|| format!("opening task store for '{team_name}'"))?;

    let project_root = find_project_root().ok();

    // ── Polling loop ──────────────────────────────────────────────────────────
    loop {
        // Check for shutdown signal in the team config.
        match cfg_store.load() {
            Ok(Some(cfg)) => {
                if let Some(me) = cfg.members.iter().find(|m| m.peer_id == peer_id) {
                    if matches!(me.status, MemberStatus::Closed) {
                        eprintln!("[teammate:{agent_name}] shutdown signal received, exiting");
                        break;
                    }
                } else {
                    // We were removed from the config - treat as shutdown.
                    eprintln!("[teammate:{agent_name}] removed from team config, exiting");
                    break;
                }
            }
            Ok(None) => {
                // Team config deleted - team was cleaned up.
                eprintln!("[teammate:{agent_name}] team '{team_name}' no longer exists, exiting");
                break;
            }
            Err(e) => {
                eprintln!("[teammate:{agent_name}] config read error: {e}");
            }
        }

        // Try to claim the next available task.
        match task_store.claim_next(&agent_name) {
            Ok(Some(task)) => {
                eprintln!(
                    "[teammate:{agent_name}] claimed task '{}' (id={})",
                    task.title, task.id
                );

                // Build a prompt that includes the task context.
                let prompt = format!(
                    "You are a teammate named '{agent_name}' on team '{team_name}'.\n\
                     Complete the following task, then provide a concise summary of \
                     what you did and the outcome.\n\n\
                     ## Task: {}\n\n{}",
                    task.title, task.description
                );

                // Use a temp file to capture the agent's final response (summary).
                let summary_path = std::env::temp_dir().join(format!("sven-task-{}.txt", task.id));

                let ci_opts = CiOptions {
                    mode: AgentMode::Agent,
                    model_override: None,
                    input: String::new(),
                    extra_prompt: Some(prompt),
                    input_from_file: false,
                    project_root: project_root.clone(),
                    output_format: OutputFormat::Compact,
                    artifacts_dir: None,
                    vars: Default::default(),
                    step_timeout_secs: None,
                    run_timeout_secs: None,
                    dry_run: false,
                    output_last_message: Some(summary_path.clone()),
                    system_prompt_file: None,
                    append_system_prompt: None,
                    trace_level: 0,
                    load_jsonl: None,
                    output_jsonl: None,
                    rerun_toolcalls: false,
                    regen_system_prompt: false,
                    max_tokens_budget: None,
                    load_chat: None,
                    output_chat: None,
                    attachments: Vec::new(),
                };

                let run_result = CiRunner::new(config.clone()).run(ci_opts).await;

                let summary = if summary_path.exists() {
                    std::fs::read_to_string(&summary_path)
                        .unwrap_or_else(|_| "(no summary)".to_string())
                } else {
                    "(no summary)".to_string()
                };
                let _ = std::fs::remove_file(&summary_path);

                match run_result {
                    Ok(()) => {
                        let _ = task_store.complete_task(&task.id, summary.trim());
                        eprintln!("[teammate:{agent_name}] completed task '{}'", task.title);
                    }
                    Err(e) => {
                        let _ = task_store.fail_task(&task.id, format!("Agent error: {e}"));
                        eprintln!("[teammate:{agent_name}] task '{}' failed: {e}", task.title);
                    }
                }
            }
            Ok(None) => {
                // No tasks available - wait before polling again.
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                eprintln!("[teammate:{agent_name}] task store error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }

    // Mark ourselves as closed on clean exit.
    let _ = cfg_store.modify(|cfg| {
        if let Some(m) = cfg.members.iter_mut().find(|m| m.peer_id == peer_id) {
            m.status = MemberStatus::Closed;
        }
    });

    Ok(())
}

async fn run_gui(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    use sven_frontend::NodeBackend as GuiNodeBackend;

    // ── Model resolution ───────────────────────────────────────────────────────
    let model_cfg = if let Some(ref model_str) = cli.model {
        sven_model::resolve_model_from_config(&config, model_str)
    } else {
        config.model.clone()
    };

    // ── Node backend (env-var / flag passthrough) ──────────────────────────────
    let node_url = std::env::var("SVEN_NODE_URL")
        .or_else(|_| std::env::var("SVEN_GATEWAY_URL"))
        .ok();
    let node_token = std::env::var("SVEN_NODE_TOKEN")
        .or_else(|_| std::env::var("SVEN_GATEWAY_TOKEN"))
        .ok();
    let node_insecure = std::env::var("SVEN_NODE_INSECURE")
        .or_else(|_| std::env::var("SVEN_GATEWAY_INSECURE"))
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let node_backend = match (node_url, node_token) {
        (Some(url), Some(token)) => Some(GuiNodeBackend {
            url,
            token,
            insecure: node_insecure,
        }),
        _ => None,
    };

    // ── Build and run the Slint GUI ────────────────────────────────────────────
    // `app.run()` is the Slint synchronous event loop and must not be called
    // from inside a tokio async task without yielding the thread first.
    // `block_in_place` tells tokio "this thread will block; move other tasks
    // elsewhere" and keeps us on the current (main) thread, which is required
    // by Slint.  We use the existing runtime handle to drive the async build
    // without creating a nested runtime.
    let opts = SvenAppOptions {
        config: Arc::clone(&config),
        model_cfg,
        mode: cli.mode,
        node_backend,
        initial_prompt: cli.prompt,
        initial_queue: vec![],
        tool_displays: sven_tools::SharedToolDisplays::default(),
    };

    tokio::task::block_in_place(|| {
        let handle = tokio::runtime::Handle::current();
        let app = handle.block_on(SvenApp::build(opts))?;
        app.run()?;
        Ok(())
    })
}

async fn run_ci(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    // ── Detect project root ──────────────────────────────────────────────────
    let project_root = find_project_root().ok();

    // ── --resume in headless mode ────────────────────────────────────────────
    if let Some(id) = &cli.resume {
        if id.is_empty() {
            anyhow::bail!(
                "--resume requires an explicit ID in headless mode.\n\
                 Use 'sven chats' to list available conversations."
            );
        }
        let file_path =
            history::resolve(id).with_context(|| format!("resolving conversation id '{id}'"))?;

        if let Some(prompt) = &cli.prompt {
            use std::fmt::Write as _;
            let current = std::fs::read_to_string(&file_path)
                .with_context(|| format!("reading {}", file_path.display()))?;
            let mut updated = current.trim_end().to_string();
            let _ = write!(updated, "\n\n## User\n\n{}\n", prompt.trim());
            std::fs::write(&file_path, &updated)
                .with_context(|| format!("appending user message to {}", file_path.display()))?;
        }

        // Legacy: resume via ConversationRunner for markdown conversation files.
        use sven_ci::{ConversationOptions, ConversationRunner};
        let content = std::fs::read_to_string(&file_path)
            .with_context(|| format!("reading {}", file_path.display()))?;
        let opts = ConversationOptions {
            mode: cli.mode,
            model_override: cli.model,
            file_path,
            content,
        };
        return ConversationRunner::new(config).run(opts).await;
    }

    // ── Resolve effective JSONL I/O paths ────────────────────────────────────
    // --file pointing to a .jsonl is treated as --load-jsonl automatically.
    let file_is_jsonl = cli
        .file
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jsonl"))
        .unwrap_or(false);

    let load_jsonl = cli.effective_load_jsonl().cloned().or_else(|| {
        if file_is_jsonl {
            cli.file.clone()
        } else {
            None
        }
    });

    let output_jsonl = cli.effective_output_jsonl().cloned();

    // ── Read workflow input ──────────────────────────────────────────────────
    // When --file points to a .jsonl, there is no separate workflow file;
    // we read from stdin (or use an empty input) for the new prompt.
    // When stdin is piped and a positional prompt is given (e.g. `cmd | sven "fix these errors"`),
    // we append stdin to the prompt with a blank line and pass that as the single user message.
    let (input, extra_prompt) = if file_is_jsonl {
        // The file is a JSONL conversation, not a workflow.  New workflow
        // input (if any) comes from stdin.
        if !is_stdin_tty() {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .context("reading stdin")?;
            (buf, cli.prompt.clone())
        } else {
            (String::new(), cli.prompt.clone())
        }
    } else if let Some(path) = &cli.file {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading input file {}", path.display()))?;
        (content, cli.prompt.clone())
    } else if !is_stdin_tty() {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .context("reading stdin")?;
        let stdin_content = buf;
        // Keep positional prompt as extra_prompt so the runner can use it when
        // stdin is a piped conversation (e.g. `sven 'plan' | sven 'summarize'`).
        // The runner merges it into the step for plain-text stdin, or uses it as
        // the new task for conversation/JSONL input.
        (stdin_content, cli.prompt.clone())
    } else {
        (String::new(), cli.prompt.clone())
    };

    // ── HSM kernel path ───────────────────────────────────────────────────────
    // Every headless run is driven by the HSM kernel. There are two kernel-backed
    // entry points and the routing below picks between them:
    //
    //   * `RuntimeRunner` — the reactive-agent single-turn path: it drives one
    //     turn to completion and streams a conversation document. It handles a
    //     fresh single prompt *and* a piped prior-conversation document replayed
    //     as history (`sven '…' | sven 'next task'`).
    //   * `CiRunner` — the multi-step workflow orchestrator, also kernel-backed
    //     (it runs every turn on the kernel via `KernelAgent`). It owns the
    //     workflow features that `RuntimeRunner` does not: markdown `--file`,
    //     JSONL, `--var` templating, `--artifacts-dir`, `--dry-run`,
    //     `--output-format json/compact`, `--output-last-message`,
    //     `--system-prompt-file`, chat load/save.
    //
    // A run that uses any of those workflow features falls through to `CiRunner`
    // to preserve them; everything else takes the `RuntimeRunner` path.
    // `sdlc`/`chat` modes always use `RuntimeRunner` (no `CiRunner` path exists).
    let mode_forces_runtime_runner = matches!(cli.mode, AgentMode::Sdlc | AgentMode::Chat);

    // Piped stdin that itself looks like a prior sven conversation document is
    // replayed as history (parsed into prior messages + a trailing pending
    // turn), not concatenated into a single prompt.
    let input_is_conversation = input.lines().any(|line| {
        matches!(
            line.trim_end(),
            "## User" | "## Sven" | "## Tool" | "## Tool Result"
        )
    });

    // The kernel `RuntimeRunner` drives one reactive-agent turn to completion.
    // It handles both a fresh single prompt *and* a piped prior-conversation
    // document replayed as history (`sven '…' | sven 'next task'`). Genuine
    // multi-step workflow features (workflow `--file`, `--var` templating,
    // `--artifacts-dir`, `--dry-run`, JSON/JSONL/compact output, chat I/O,
    // `--system-prompt-file`, `--output-last-message`) live in `CiRunner`; a run
    // using any of them falls through to preserve those features. `sdlc`/`chat`
    // modes always use `RuntimeRunner`.
    let workflow_features_absent = cli.file.is_none()
        && matches!(cli.output_format, OutputFormatArg::Conversation)
        && cli.artifacts_dir.is_none()
        && !cli.dry_run
        && cli.output_last_message.is_none()
        && cli.system_prompt_file.is_none()
        && cli.vars.is_empty()
        && cli.effective_load_chat().is_none()
        && cli.effective_output_chat().is_none()
        && cli.effective_output_jsonl().is_none();

    if load_jsonl.is_none() && (mode_forces_runtime_runner || workflow_features_absent) {
        let kernel_mode = std::env::var("SVEN_MODE").unwrap_or_else(|_| {
            match cli.mode {
                AgentMode::Chat => "chat",
                AgentMode::Sdlc => "sdlc",
                _ => "agent",
            }
            .to_string()
        });
        // Resolve the new prompt and any prior history to replay.
        //
        // When stdin is a prior sven conversation document, parse it into
        // history + a trailing pending user turn. The new task is the CLI
        // positional prompt (if any), else the pending turn. History is seeded
        // into the kernel thread so the turn sees full context. Otherwise stdin
        // is plain text: trim it (notably the trailing newline piped stdin
        // always carries so exact-match model routing sees `"ping"`, not
        // `"ping\n"`) and merge with any positional prompt.
        let (prompt, history) = if input_is_conversation {
            match sven_input::parse_conversation(&input) {
                Ok(conv) => {
                    let new_task = extra_prompt
                        .as_ref()
                        .map(|p| p.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .or(conv.pending_user_input);
                    match new_task {
                        Some(task) => (task, conv.history),
                        None => {
                            eprintln!(
                                "[sven:error] Piped conversation has no pending task.\n\
                                 \n\
                                 To continue a piped conversation provide a prompt:\n\
                                 \n\
                                 \tsven 'task1' | sven 'task2'\n\
                                 \n\
                                 Or end the piped output with an unanswered ## User section\n\
                                 so the next sven instance picks it up automatically."
                            );
                            std::process::exit(2);
                        }
                    }
                }
                // If the document fails to parse, fall back to treating stdin as
                // a plain single prompt rather than losing the run.
                Err(e) => {
                    eprintln!(
                        "[sven:warn] Failed to parse piped input as conversation ({e}), \
                         treating as plain prompt"
                    );
                    let prompt = match &extra_prompt {
                        Some(p) if !input.trim().is_empty() => {
                            format!("{}\n\n{}", input.trim(), p.trim())
                        }
                        Some(p) => p.trim().to_string(),
                        None => input.trim().to_string(),
                    };
                    (prompt, Vec::new())
                }
            }
        } else {
            let prompt = match &extra_prompt {
                Some(p) if !input.trim().is_empty() => {
                    format!("{}\n\n{}", input.trim(), p.trim())
                }
                Some(p) => p.trim().to_string(),
                None => input.trim().to_string(),
            };
            (prompt, Vec::new())
        };
        // Apply the `--model` override into the config the kernel builds from
        // (the legacy CiRunner does the same before constructing its agent).
        let kernel_config = if let Some(m) = &cli.model {
            let mut cfg = (*config).clone();
            cfg.model = sven_model::resolve_model_from_config(&cfg, m);
            Arc::new(cfg)
        } else {
            config.clone()
        };
        let runner = sven_ci::RuntimeRunner::new(kernel_config);
        let code = runner
            .run(sven_ci::RuntimeRunnerOptions {
                mode: kernel_mode,
                prompt,
                history,
                project_root: project_root.clone(),
                timeout_secs: cli.run_timeout,
                step_timeout_secs: cli.step_timeout,
                max_tokens_budget: cli.max_tokens,
                append_system_prompt: cli.append_system_prompt.clone(),
                trace_level: cli.verbose,
            })
            .await;
        std::process::exit(code);
    }

    // ── Parse template variables ──────────────────────────────────────────────
    let mut vars: HashMap<String, String> = HashMap::new();
    for spec in &cli.vars {
        if let Some((k, v)) = sven_ci::template::parse_var(spec) {
            vars.insert(k, v);
        } else {
            eprintln!(
                "[sven:warn] Ignoring invalid --var argument: {spec:?}  (expected KEY=VALUE)"
            );
        }
    }

    // ── Map CLI output format ─────────────────────────────────────────────────
    let output_format = match cli.output_format {
        OutputFormatArg::Conversation => OutputFormat::Conversation,
        OutputFormatArg::Json => OutputFormat::Json,
        OutputFormatArg::Compact => OutputFormat::Compact,
        OutputFormatArg::Jsonl => OutputFormat::Jsonl,
    };

    let load_chat = cli.effective_load_chat().cloned();
    let output_chat = cli.effective_output_chat().cloned();

    let input_from_file = cli.file.is_some() && !file_is_jsonl;

    let opts = CiOptions {
        mode: cli.mode,
        model_override: cli.model,
        input,
        extra_prompt,
        input_from_file,
        project_root,
        output_format,
        artifacts_dir: cli.artifacts_dir,
        vars,
        step_timeout_secs: cli.step_timeout,
        run_timeout_secs: cli.run_timeout,
        dry_run: cli.dry_run,
        output_last_message: cli.output_last_message,
        system_prompt_file: cli.system_prompt_file,
        append_system_prompt: cli.append_system_prompt,
        trace_level: cli.verbose,
        load_jsonl,
        output_jsonl,
        rerun_toolcalls: cli.rerun_toolcalls,
        regen_system_prompt: cli.regen_system_prompt,
        max_tokens_budget: cli.max_tokens,
        load_chat,
        output_chat,
        attachments: cli.attach,
    };

    CiRunner::new(config).run(opts).await
}

async fn run_tui(cli: Cli, config: Arc<sven_config::Config>) -> anyhow::Result<()> {
    use ratatui::crossterm::{
        event::{
            DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
            PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
    };

    let initial_history = match &cli.resume {
        None => None,
        Some(id) => {
            let actual_id = if id.is_empty() {
                match pick_chat_with_fzf()? {
                    Some(picked) => picked,
                    None => return Ok(()),
                }
            } else {
                id.clone()
            };

            let (parsed, path) = history::load(&actual_id)
                .with_context(|| format!("loading conversation '{actual_id}'"))?;

            let segments: Vec<sven_tui::ChatSegment> = parsed
                .history
                .into_iter()
                .map(sven_tui::ChatSegment::Message)
                .collect();
            Some((segments, path))
        }
    };

    // Install a panic hook that restores the terminal to a usable state before
    // printing the panic message.  Without this, a panic while in raw-mode /
    // alternate-screen leaves the terminal permanently garbled.
    // Use stdout (same fd as ratatui) - stderr may be redirected to /dev/null
    // below so escape sequences written there would never reach the terminal.
    {
        use ratatui::crossterm::{
            event::DisableMouseCapture,
            execute,
            terminal::{disable_raw_mode, LeaveAlternateScreen},
        };
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture,);
            original_hook(info);
        }));
    }

    let terminal = ratatui::init();
    // Setup escape sequences go to stderr.  ratatui owns stdout (via its
    // CrosstermBackend) and may buffer/reorder writes; using the independent
    // stderr fd avoids that.  Stderr still points to the real terminal here
    // because the dup2 redirect below has not happened yet.
    let _ = execute!(std::io::stderr(), EnableMouseCapture);
    let _ = execute!(
        std::io::stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    );

    // Redirect stderr to /dev/null (or SVEN_LOG_FILE) AFTER setup is done.
    // From this point on stderr is a sink; all cleanup escape sequences use
    // stdout instead (see below).  This is the defence against subprocess
    // output corrupting the TUI: any process that inherits our stderr fd
    // writes to /dev/null instead of the raw terminal.
    // Tracing is already suppressed via LevelFilter::OFF above; this catches
    // anything else (dynamic libraries, C extensions, etc.).
    #[cfg(unix)]
    {
        use std::os::unix::io::IntoRawFd;
        let sink_path = std::env::var("SVEN_LOG_FILE").unwrap_or_else(|_| "/dev/null".to_string());
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&sink_path)
        {
            unsafe {
                let fd = f.into_raw_fd();
                libc::dup2(fd, libc::STDERR_FILENO);
                libc::close(fd);
            }
        }
    }
    // On non-Unix platforms (e.g. Windows), stderr redirection via dup2 is not
    // available without platform-specific APIs. Tracing is suppressed via
    // LevelFilter::OFF above, which is sufficient for TUI mode.
    #[cfg(not(unix))]
    {
        let _ = std::env::var("SVEN_LOG_FILE");
    }

    // Spawn a background task that listens for SIGTERM / SIGINT from the OS
    // (e.g. `kill <pid>` or systemd shutdown).  These signals bypass the
    // normal Rust panic/drop machinery, so we must handle them explicitly to
    // restore the terminal before the process exits.  In raw-mode, Ctrl-C is
    // received as a key event and handled by the TUI; real SIGINT only arrives
    // when the process is sent the signal from outside.
    // Uses stdout for all escape sequences (stderr is now /dev/null).
    tokio::spawn(async move {
        use ratatui::crossterm::{
            event::DisableMouseCapture,
            execute,
            terminal::{disable_raw_mode, LeaveAlternateScreen},
        };
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut sigint = match signal(SignalKind::interrupt()) {
                Ok(s) => s,
                Err(_) => return,
            };
            tokio::select! {
                _ = sigterm.recv() => {}
                _ = sigint.recv()  => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen, DisableMouseCapture,);
        std::process::exit(1);
    });

    // ── Load workflow into initial TUI queue ─────────────────────────────────
    // If --file points to a markdown workflow, parse the steps and push them
    // into the TUI queue so the user can review them before they are sent.
    // The file must NOT be a JSONL file; JSONL is handled via --load-jsonl.
    let file_is_jsonl = cli
        .file
        .as_ref()
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("jsonl"))
        .unwrap_or(false);

    let initial_queue: Vec<QueuedMessage> = if let Some(path) = &cli.file {
        if !file_is_jsonl {
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let (fm, body) = parse_frontmatter(&content);
                    let _ = fm; // Frontmatter used by runner, not TUI queue loader
                    let config_ref = config.clone();
                    let mut wf = parse_workflow(body);
                    let mut q = Vec::new();
                    while let Some(step) = wf.steps.pop() {
                        // Resolve per-step model string into a ModelDirective
                        let model_transition = step.options.model.as_deref().map(|name| {
                            let cfg = sven_model::resolve_model_from_config(&config_ref, name);
                            ModelDirective::SwitchTo(Box::new(cfg))
                        });
                        // Resolve per-step mode string into an AgentMode
                        let mode_transition = step.options.mode.as_deref().and_then(|m| match m {
                            "research" => Some(AgentMode::Research),
                            "plan" => Some(AgentMode::Plan),
                            "agent" => Some(AgentMode::Agent),
                            "chat" => Some(AgentMode::Chat),
                            "sdlc" => Some(AgentMode::Sdlc),
                            _ => None,
                        });
                        q.push(QueuedMessage {
                            content: step.content,
                            model_transition,
                            mode_transition,
                        });
                    }
                    q
                }
                Err(e) => {
                    eprintln!(
                        "[sven:warn] Could not read workflow file {}: {e}",
                        path.display()
                    );
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    // Resolve JSONL paths for TUI: --load-jsonl feeds initial history; output
    // goes to --output-jsonl (or --jsonl which combines both).
    let jsonl_load_path = cli.effective_load_jsonl().cloned();
    let jsonl_save_path = cli.effective_output_jsonl().cloned();

    // Auto-detect node-proxy mode: when SVEN_NODE_URL and SVEN_NODE_TOKEN
    // are present (injected by the node into web PTY sessions), connect the
    // TUI to the running node so the agent has full P2P peer access.
    let node_backend = {
        let url = std::env::var("SVEN_NODE_URL")
            .or_else(|_| std::env::var("SVEN_GATEWAY_URL"))
            .ok();
        let token = std::env::var("SVEN_NODE_TOKEN")
            .or_else(|_| std::env::var("SVEN_GATEWAY_TOKEN"))
            .ok();
        let insecure = std::env::var("SVEN_NODE_INSECURE")
            .or_else(|_| std::env::var("SVEN_GATEWAY_INSECURE"))
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        match (url, token) {
            (Some(url), Some(token)) => Some(NodeBackend {
                url,
                token,
                insecure,
            }),
            _ => None,
        }
    };

    let chat_load_path = cli.effective_load_chat().cloned();
    let chat_output_path = cli.effective_output_chat().cloned();

    let opts = AppOptions {
        mode: cli.mode,
        initial_prompt: cli.prompt,
        initial_history,
        no_nvim: !cli.nvim,
        model_override: cli.model,
        jsonl_path: jsonl_save_path,
        jsonl_load_path,
        initial_queue,
        node_backend,
        chat_path: chat_load_path,
        output_chat_path: chat_output_path,
    };

    let app = App::new(config, opts);
    let result = app.run(terminal).await;

    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}

/// Environment variable set by task_tool when spawning a subagent.
/// When set, stdout is reserved for ACP; we suppress all tracing to avoid
/// any accidental pollution of the protocol stream.
const SUBAGENT_DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

fn init_logging(verbosity: u8, is_tui: bool, is_node: bool) {
    // In TUI mode tracing output written to stderr corrupts the ratatui
    // display.  When running as a subagent (SVEN_SUBAGENT_DEPTH set), stdout
    // is reserved for ACP and must not be polluted by any printouts.
    // We suppress all logging unless the caller opts in:
    //   • Set SVEN_LOG_FILE=/path/to/file  → logs go to that file (any mode)
    //   • Set RUST_LOG=...                 → respects the env filter
    //   • Pass --verbose (-v)              → enables debug/trace (headless only)
    let is_subagent = std::env::var(SUBAGENT_DEPTH_ENV).is_ok();
    if is_tui || is_subagent {
        // Check for an explicit log file - advanced debugging only.
        if let Ok(log_path) = std::env::var("SVEN_LOG_FILE") {
            use std::sync::Mutex;
            if let Ok(file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
            {
                let filter =
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"));
                let _ = tracing_subscriber::registry()
                    .with(
                        fmt::layer()
                            .with_target(true)
                            .with_ansi(false)
                            .with_writer(Mutex::new(file)),
                    )
                    .with(filter)
                    .try_init();
                return;
            }
        }
        // No log file: suppress all output so the TUI is not corrupted,
        // or so the subagent's stdout (ACP) is not polluted.
        let _ = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::OFF)
            .try_init();
        return;
    }

    let level = match verbosity {
        0 if is_node => "info",
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    let layer = fmt::layer()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_timer(fmt::time::uptime());

    let _ = tracing_subscriber::registry()
        .with(layer)
        .with(filter)
        .try_init();
}

fn is_stdin_tty() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}
