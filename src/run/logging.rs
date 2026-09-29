// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use tracing_subscriber::{filter::EnvFilter, fmt, prelude::*};

/// Environment variable set by task_tool when spawning a subagent.
/// When set, stdout is reserved for ACP; we suppress all tracing to avoid
/// any accidental pollution of the protocol stream.
const SUBAGENT_DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

pub(crate) fn init_logging(verbosity: u8, is_tui: bool, is_node: bool) {
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
        // No ANSI colour codes when stderr is piped (CI logs, e2e greps).
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_timer(fmt::time::uptime());

    let _ = tracing_subscriber::registry()
        .with(layer)
        .with(filter)
        .try_init();
}
