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
        /// WebSocket URL of a running `sven node` to proxy agent calls through.
        ///
        /// When provided, the ACP server connects to the node over WebSocket
        /// and forwards every session request to it.  The node's agent, tool
        /// registry, and P2P capabilities are all exposed to the IDE.
        ///
        /// Example: --node-url wss://127.0.0.1:18790/ws
        #[arg(long, value_name = "URL")]
        node_url: Option<String>,

        /// Bearer token for authenticating with the sven node.
        ///
        /// Required when `--node-url` is set.  This is the raw token printed by
        /// `sven node start` on first launch (not the hash stored on disk).
        ///
        /// May also be provided via the SVEN_NODE_TOKEN environment variable.
        #[arg(long, env = "SVEN_NODE_TOKEN", value_name = "TOKEN")]
        token: Option<String>,

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

        /// Extra PEM CA certificate file to trust when verifying the node's
        /// TLS certificate (e.g. a remote node's `ca-cert.pem`).
        ///
        /// The system trust store and the local node CA
        /// (`~/.config/sven/node/tls/ca-cert.pem`) are always trusted.
        #[arg(long, value_name = "FILE")]
        node_ca: Option<std::path::PathBuf>,

        /// DANGER: skip TLS certificate verification when connecting to the
        /// node. Local testing against a `self-signed` node only - never use
        /// this in production.
        #[arg(long)]
        insecure_tls: bool,
    },
}

/// Runs a `sven acp` subcommand.
pub async fn run_acp_command(cmd: &AcpCommands) -> anyhow::Result<()> {
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
                crate::serve_stdio_node_proxy_with_options(url.clone(), tok, options).await
            } else {
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
}
