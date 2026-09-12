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

    /// Check everything the local learning pipeline needs before it can run
    /// real work, naming every problem at once rather than stopping at the
    /// first one `sven learn flush` happens to hit.
    ///
    ///   sven learn doctor
    Doctor,
}
