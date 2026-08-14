// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Team subcommand ───────────────────────────────────────────────────────────

/// `sven team` subcommands - manage agent teams.
///
/// Agent teams allow multiple sven instances to collaborate on a shared task
/// list.  A team is created once (via `create` or `start`) and persists in
/// `~/.config/sven/teams/` until cleaned up.
///
/// Quick start:
///
///   # Create a team from a definition file
///   sven team init --name audit
///   sven team start --file .sven/teams/audit.yaml
///
///   # Monitor progress
///   sven team status audit
///
///   # Clean up when done
///   sven team cleanup audit --force
#[derive(Subcommand, Debug)]
pub enum TeamCommands {
    /// List all known teams.
    List,

    /// Print detailed status for a team.
    Status {
        /// Team name.
        name: String,
    },

    /// Create a new team directory.
    ///
    /// This creates the persistent team configuration.  To also spawn
    /// agent processes from a definition file, use `sven team start`.
    Create {
        /// Team name (alphanumeric + hyphens/underscores).
        #[arg(long)]
        name: String,
        /// Optional description of the team's goal.
        #[arg(long)]
        goal: Option<String>,
        /// Maximum simultaneous active teammates (default: 8).
        #[arg(long, default_value = "8")]
        max_active: usize,
        /// Global token budget (0 = unlimited).
        #[arg(long, default_value = "0")]
        token_budget: u64,
    },

    /// Launch agents from a team definition YAML file.
    ///
    /// Each member in the definition is spawned as a separate sven process.
    ///
    /// Example:
    ///   sven team start --file .sven/teams/code-review.yaml
    Start {
        /// Path to a team definition YAML file.
        #[arg(long, short = 'f')]
        file: std::path::PathBuf,
        /// Path to the sven binary to spawn (defaults to current executable).
        #[arg(long)]
        sven_bin: Option<String>,
        /// Print commands without executing them.
        #[arg(long)]
        dry_run: bool,
    },

    /// Remove a team's configuration directory.
    Cleanup {
        /// Team name.
        name: String,
        /// Skip confirmation and force removal.
        #[arg(long)]
        force: bool,
    },

    /// List team definition files in the current project.
    ///
    /// Scans `.sven/teams/*.yaml` in the current directory.
    Definitions,

    /// Create a starter team definition file.
    ///
    /// Writes `.sven/teams/<NAME>.yaml` with a sensible default structure.
    Init {
        /// Team name.
        #[arg(long)]
        name: String,
        /// Optional team goal.
        #[arg(long)]
        goal: Option<String>,
    },

    /// Watch a team's live event stream.
    ///
    /// Polls the team status and prints updates as they arrive, including
    /// member status changes, task completions, and budget warnings.
    ///
    /// Example:
    ///   sven team watch security-audit
    Watch {
        /// Team name to watch.
        name: String,
        /// Refresh interval in seconds (default: 3).
        #[arg(long, default_value = "3")]
        interval: u64,
        /// Exit after this many seconds (0 = run forever).
        #[arg(long, default_value = "0")]
        timeout: u64,
    },
}
