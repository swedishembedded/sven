// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `sven acp` clap grammar and handler, reusable both by the monolithic
//! `sven` binary's `Commands::Acp` dispatch and the standalone `sven-acp`
//! binary (Phase 6.2 of the refactor plan).

use clap::Subcommand;

/// `sven acp` subcommands.
#[derive(Subcommand, Debug)]
pub enum AcpCommands {
    /// Expose sven as a full ACP agent over stdio.
    ///
    /// Starts an Agent Client Protocol server that speaks JSON-RPC 2.0 on
    /// stdin/stdout.  Any ACP-compatible IDE (JetBrains, Zed, VS Code with an
    /// ACP extension) can launch sven as a subprocess and interact with it as
    /// a first-class AI coding agent with sessions, streaming, plans, and mode
    /// switching.
    ///
    ///   JetBrains / Zed / VS Code (`acp.json`):
    ///
    ///   { "agents": { "sven": { "command": "sven", "args": ["acp", "serve"] } } }
    ///
    /// The server blocks until stdin reaches EOF (i.e. until the IDE
    /// disconnects the subprocess).
    Serve {
        /// Override the model used by this ACP server instance.
        ///
        /// Accepts a bare model name ("claude-sonnet-4-6") or a
        /// "provider/model" pair ("anthropic/claude-sonnet-4-6").
        ///
        /// The task tool passes the parent agent's current model here so that
        /// sub-agents automatically inherit the same model.
        ///
        /// May also be set via the SVEN_MODEL environment variable.
        #[arg(long, env = "SVEN_MODEL", value_name = "MODEL")]
        model: Option<String>,

        /// Override only the provider without changing the model name.
        ///
        /// May also be set via the SVEN_PROVIDER environment variable.
        #[arg(long, env = "SVEN_PROVIDER", value_name = "PROVIDER")]
        provider: Option<String>,
    },
}

/// Runs a `sven acp` subcommand.
pub async fn run_acp_command(cmd: &AcpCommands) -> anyhow::Result<()> {
    match cmd {
        AcpCommands::Serve { model, provider } => {
            let mut config = sven_config::load(None)?;
            if let Some(ref name) = model {
                config.model = sven_model::resolve_model_from_config(&config, name);
            }
            if let Some(ref prov) = provider {
                config.model.provider = prov.clone();
            }
            crate::serve_stdio(std::sync::Arc::new(config)).await
        }
    }
}
