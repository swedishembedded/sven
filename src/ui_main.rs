// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Entry point for the `sven-ui` desktop GUI binary.
//!
//! Parses a minimal CLI subset (the TUI CLI flags that are meaningful for a
//! desktop UI), loads configuration, and runs the Slint-based GUI.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use sven_config::AgentMode;
use sven_gui::bridge::{SvenApp, SvenAppOptions};

/// Sven Desktop GUI — cross-platform AI coding agent interface.
#[derive(Parser, Debug)]
#[command(name = "sven-ui", about = "Sven Desktop GUI", long_about = None)]
struct UiCli {
    /// Initial prompt to send on startup.
    #[arg(short = 'p', long)]
    prompt: Option<String>,

    /// Override the model (e.g. `anthropic/claude-opus-4-5`).
    #[arg(short = 'm', long)]
    model: Option<String>,

    /// Agent mode: agent, research, plan.
    #[arg(long, default_value = "agent")]
    mode: String,

    /// Path to config file (default: ~/.config/sven/config.toml).
    #[arg(long)]
    config: Option<String>,

    /// Connect to a running sven node (overrides `SVEN_NODE_URL`).
    #[arg(long)]
    node_url: Option<String>,

    /// Bearer token for node access (overrides `SVEN_NODE_TOKEN`).
    #[arg(long)]
    node_token: Option<String>,

    /// Skip TLS certificate verification for node connections.
    #[arg(long)]
    insecure: bool,

    /// Enable verbose logging.
    #[arg(short = 'v', long)]
    verbose: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = UiCli::parse();

    // ── Logging ───────────────────────────────────────────────────────────────
    let log_level = if cli.verbose { "debug" } else { "info" };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .try_init();

    // ── Config ────────────────────────────────────────────────────────────────
    let config_path = cli.config.as_deref().map(Path::new);
    let config = sven_config::load(config_path).context("failed to load sven configuration")?;
    let config = Arc::new(config);

    // ── Model resolution ──────────────────────────────────────────────────────
    let model_cfg = if let Some(ref model_str) = cli.model {
        sven_model::resolve_model_from_config(&config, model_str)
    } else {
        config.model.clone()
    };

    // ── Agent mode ────────────────────────────────────────────────────────────
    let mode = match cli.mode.as_str() {
        "agent" | "a" => AgentMode::Agent,
        "research" | "r" => AgentMode::Research,
        "plan" | "p" => AgentMode::Plan,
        other => {
            eprintln!("unknown mode: {other}; using 'agent'");
            AgentMode::Agent
        }
    };

    // ── Node backend ──────────────────────────────────────────────────────────
    let node_url = cli.node_url.or_else(|| std::env::var("SVEN_NODE_URL").ok());
    let node_token = cli
        .node_token
        .or_else(|| std::env::var("SVEN_NODE_TOKEN").ok());
    let node_insecure = cli.insecure
        || std::env::var("SVEN_GATEWAY_INSECURE")
            .or_else(|_| std::env::var("SVEN_NODE_INSECURE"))
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);

    let node_backend = node_url.and_then(|url| {
        node_token.map(|token| sven_frontend::NodeBackend {
            url,
            token,
            insecure: node_insecure,
        })
    });

    // ── Build tokio runtime and run the GUI ───────────────────────────────────
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    // Enter the runtime's context so that `tokio::spawn` works from within
    // the synchronous Slint event loop callbacks.
    let _guard = runtime.enter();

    let app = runtime.block_on(async {
        SvenApp::build(SvenAppOptions {
            config: Arc::clone(&config),
            model_cfg,
            mode,
            node_backend,
            initial_prompt: cli.prompt,
            initial_queue: vec![],
            tool_displays: sven_tools::SharedToolDisplays::default(),
        })
        .await
    })?;

    app.run()?;

    Ok(())
}
