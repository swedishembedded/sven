// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Task subcommand ───────────────────────────────────────────────────────────

/// `sven task` subcommands - run a single task whose completion is checked
/// by a declarative verifier, not by asking the model whether it is done.
#[derive(Subcommand, Debug)]
pub enum TaskCommands {
    /// Attempt a `.task.toml` file to completion, verifying every claim.
    ///
    /// Reads the file, freezes its verifier (pinning a content digest of the
    /// file bytes as the verifier's origin - see `sven_vocab::verify`),
    /// hands it to the verified-task machine, and blocks until it either
    /// passes, exhausts its retry budget, or parks needing a human. Unlike
    /// every other headless mode, the model's own claim of completion is
    /// never trusted on its own - an independent check decides.
    ///
    /// Example, a task file:
    ///
    ///     id = "write-readme"
    ///     prompt = "Write a one-paragraph README.md describing this crate."
    ///     max_attempts = 2
    ///
    ///     [verifier]
    ///     kind = "file_exists"
    ///     path = "README.md"
    ///     min_bytes = 20
    ///
    ///     sven task run write-readme.task.toml
    // Verbatim, so `--help` keeps the example file's lines apart.
    #[command(verbatim_doc_comment)]
    Run {
        /// Path to the `.task.toml` file.
        file: std::path::PathBuf,
        /// Project root the verifier's paths resolve against (defaults to
        /// the current directory).
        #[arg(long)]
        project_root: Option<std::path::PathBuf>,
        /// Model override, same spelling as the top-level `--model`.
        #[arg(long)]
        model: Option<String>,
        /// Per-run timeout in seconds (all attempts combined).
        #[arg(long)]
        timeout: Option<u64>,
    },
}
