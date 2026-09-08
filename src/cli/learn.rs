// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Learn subcommand ──────────────────────────────────────────────────────────

/// `sven learn` subcommands - drive the pending-facts drain by hand.
///
/// The drain normally runs as a background task on a timer, which is right for
/// an interactive session that stays open. These are for the case that has no
/// next tick: a script that runs one agent and exits.
#[derive(Subcommand, Debug)]
pub enum LearnCommands {
    /// Submit every pending fact now and block until each has a real outcome.
    ///
    /// Blocks on the submitter's own verdicts - a training study can take
    /// minutes - never on a fixed sleep, and prints one line per fact:
    /// `promoted`, `rejected` with the reason it was turned down, or `failed`
    /// when the pipeline itself broke and nothing was decided.
    ///
    /// Exits non-zero only if a fact `failed`: a rejection is a real answer,
    /// and a script that treats "the model did not learn this" as a build
    /// failure would be wrong.
    ///
    /// Example, in a shell script that has to finish learning before it exits:
    ///
    ///   sven --headless "learn what you can from spec.md" && sven learn flush
    Flush {
        /// Print the outcomes as a JSON array instead of one line per fact.
        #[arg(long)]
        json: bool,
    },

    /// Export recorded trajectories whose stamped outcome cleared a reward
    /// threshold into brain's training-chat JSONL format - the procedural
    /// (task-completion) counterpart to the fact/document loop above.
    ///
    /// A trajectory with no stamped reward at all (unconcluded, or never
    /// scored) is skipped, never treated as a zero. Overwrites `--out`
    /// rather than appending, so a repeated export over the same runs
    /// directory is idempotent.
    ///
    /// Example: export every successful headless run's own trajectory log
    /// into a dataset for a future trajectory-based study:
    ///
    ///   sven learn export-trajectories --runs .sven/logs --min-reward 0.8 --out dataset.jsonl
    ExportTrajectories {
        /// Directory to scan for `*.atif.json` trajectory files.
        #[arg(long)]
        runs: std::path::PathBuf,
        /// Minimum stamped reward (`final_metrics.extra.reward`) a
        /// trajectory must have to be exported.
        #[arg(long, default_value_t = 1.0)]
        min_reward: f64,
        /// Where to write the JSONL dataset.
        #[arg(long)]
        out: std::path::PathBuf,
    },
}
