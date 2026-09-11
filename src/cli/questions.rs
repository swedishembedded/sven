// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Questions subcommand ────────────────────────────────────────────────────

/// `sven questions` subcommands - the operator side of the async
/// question-parking primitive.
///
/// A headless run that cannot answer a question itself parks it (exit code 5,
/// `EXIT_NEEDS_HUMAN`) rather than blocking or guessing. These commands are
/// how a human sees and answers what is parked.
#[derive(Subcommand, Debug)]
pub enum QuestionsCommands {
    /// List every parked question with no recorded answer yet.
    List {
        /// Print as a JSON array instead of one line per question.
        #[arg(long)]
        json: bool,
    },

    /// Record a human's answer to a parked question.
    ///
    /// Durably records the answer (append-only, hash-chained) so it survives
    /// past this process. Does **not** itself resume the session that asked
    /// it - resuming a parked kernel session from its recorded answer is not
    /// wired up yet; see the trajectory's `--resume` path once that lands.
    ///
    /// Example:
    ///
    ///   sven questions list
    ///   sven questions answer 3f9c2b1a-... "Axum"
    Answer {
        /// The question id, as shown by `sven questions list`.
        question_id: String,
        /// The answer text.
        answer: String,
    },
}
