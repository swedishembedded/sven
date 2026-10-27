// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The arguments of `sven rc`.
use std::path::PathBuf;

use clap::{Args, Subcommand};
use uuid::Uuid;

/// `sven rc`: where the remote sessions work, or a conversation to read.
#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct RcArgs {
    /// Where the sessions work; the current directory by default.
    #[arg(long)]
    pub working_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub action: Option<RcAction>,
}

#[derive(Subcommand, Debug)]
pub enum RcAction {
    /// Print a conversation that runs on this machine, as the server keeps
    /// it; with --follow, keep printing what is added from anywhere until
    /// Ctrl-C.
    ///   sven rc show 6f1c...e2 --follow
    Show {
        /// The conversation's id (the session id `sven rc` logs).
        conversation: Uuid,
        #[arg(long)]
        follow: bool,
    },
}
