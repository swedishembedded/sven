// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Index subcommand ──────────────────────────────────────────────────────────

/// `sven index` subcommands - manage the repository context index.
#[derive(Subcommand, Debug)]
pub enum IndexCommands {
    /// Build or rebuild the repository context index.
    ///
    /// Scans the repository and extracts the file tree, public API symbols,
    /// and import graph.  Stores the result in `.sven/index/index.json`.
    ///
    /// Run this once after cloning and after large-scale refactors.
    Build {
        /// Suppress progress output (structured JSON is still written to stdout).
        #[arg(long)]
        quiet: bool,
    },

    /// Search the index for symbols matching a query.
    ///
    /// Case-insensitive substring match against symbol names and signatures.
    ///
    /// Example:
    ///   sven index query "authenticate"
    Query {
        /// Search query (case-insensitive substring match).
        query: String,
        /// Maximum results to show (default: 20).
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Show statistics about the current index.
    Stats,
}

