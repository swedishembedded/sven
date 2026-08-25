// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Standalone `sven-mcp` binary (refactor plan Phase 6.2): the same MCP
//! server as `sven mcp`, without linking the TUI closure of the
//! monolithic `sven` binary.

use clap::Parser;
use sven_mcp::cli::{run_mcp_command, McpCommands};

#[derive(Debug, Parser)]
#[command(name = "sven-mcp", version, about = "MCP server for sven")]
struct Cli {
    #[command(subcommand)]
    command: McpCommands,

    /// Increase verbosity (-v = debug, -vv = trace)
    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // stdout is the JSON-RPC transport - tracing must never write there.
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_env_filter(filter)
        .init();

    let _ = rustls::crypto::ring::default_provider().install_default();

    run_mcp_command(&cli.command).await
}
