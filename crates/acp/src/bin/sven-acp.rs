// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Standalone `sven-acp` binary (refactor plan Phase 6.2): the same ACP
//! agent server as `sven acp`, without linking the TUI/P2P/cloud closure of
//! the monolithic `sven` binary.

use clap::Parser;
use sven_acp::cli::{run_acp_command, AcpCommands};

#[derive(Debug, Parser)]
#[command(name = "sven-acp", version, about = "ACP agent server for sven")]
struct Cli {
    #[command(subcommand)]
    command: AcpCommands,

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

    run_acp_command(&cli.command).await
}
