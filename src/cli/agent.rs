// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Agent subcommand ──────────────────────────────────────────────────────────

/// `sven agent` subcommands - the shell-level form of the SDK.
///
/// Where the interactive TUI holds a session open, these advance an agent by
/// one step per process and keep everything durable in a state file. That is
/// the same shape a service uses per request, which makes it possible to
/// exercise the framework's lifecycle from a shell script.
///
/// Examples:
///
///   sven agent step "what does this repo do?"
///   sven agent step --state ./review.json "read src/lib.rs and summarise it"
///   sven agent step --state ./review.json "now list its public types"
#[derive(Subcommand, Debug)]
pub enum AgentCommands {
    /// Advance an agent by exactly one step, then exit.
    ///
    /// With `--state`, the agent is loaded from that file if it exists and
    /// written back to it afterwards, so successive invocations continue one
    /// conversation. Without it the step is a one-off and nothing is kept.
    ///
    /// Examples:
    ///
    ///   sven agent step "summarise the build failure"
    ///   sven agent step --state ./s.json --role "You are terse." "hello"
    Step {
        /// File the agent's state is loaded from and written back to.
        ///
        /// Created on first use. Its absence means a fresh agent; a file that
        /// exists but cannot be read is an error rather than a fresh start,
        /// since silently discarding a conversation is worse than failing.
        #[arg(long)]
        state: Option<std::path::PathBuf>,

        /// Machine to run. Only used when starting a fresh agent - a resumed
        /// one keeps the mode it was created with.
        #[arg(long, default_value = "agent")]
        mode: String,

        /// Stable role for the agent, used as its system prompt.
        #[arg(long)]
        role: Option<String>,

        /// Approve every permission gate instead of refusing it.
        ///
        /// Off by default: a step running unattended should not grant a
        /// dangerous capability on nobody's behalf.
        #[arg(long)]
        yes: bool,

        /// The message to send.
        message: String,
    },
}
