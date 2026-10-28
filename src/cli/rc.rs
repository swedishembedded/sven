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

    /// Run as a server's hosted host, in --sandbox DIR or in a --container.
    /// The sign-in is read from standard input (written there by the server
    /// that starts it).
    #[arg(long)]
    pub hosted: bool,

    /// A hosted host's own directory, and the only one it can reach: its
    /// kept sessions (`sessions/`) and where its sessions work (`work/`).
    /// One per hosted host; never shared. Sessions get only tools that never
    /// touch this machine (web search, web fetch, finding skills), and the
    /// kernel confines the whole process to this directory.
    #[arg(long, requires = "hosted", conflicts_with_all = ["working_dir", "container"])]
    pub sandbox: Option<PathBuf>,

    /// A hosted host inside a sandbox container the server started for one
    /// conversation: sessions work in /workspace with a shell and file
    /// tools, /artifacts holds the account's shared files, and --state what
    /// the host keeps. The container is the boundary.
    #[arg(long, requires = "hosted", conflicts_with = "working_dir")]
    pub container: bool,

    /// Inside --container: where the host keeps what it must between runs
    /// (its sessions), on the conversation's own disk.
    #[arg(long, requires = "container")]
    pub state: Option<PathBuf>,

    /// Inside --container: the server's egress socket, the only way out.
    /// Every HTTP client in the container is pointed at it as its proxy.
    #[arg(long, requires = "container")]
    pub egress_socket: Option<PathBuf>,

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
    /// Attach this terminal to a conversation that runs on this machine:
    /// print it as it changes, and send each line typed as the next
    /// message, as if typed in the server's interface. Ctrl-C or end of
    /// input detaches; the conversation goes on. --tui shows it in the
    /// full-screen UI.
    ///   sven rc attach 6f1c...e2 [--tui]
    Attach {
        /// The conversation's id (the session id `sven rc` logs).
        conversation: Uuid,
        /// In the full-screen UI, as if the conversation ran here.
        #[arg(long)]
        tui: bool,
    },
}
