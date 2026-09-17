// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use clap::Subcommand;

// ── Tool subcommand ───────────────────────────────────────────────────────────

/// `sven tool` subcommands.
///
/// Run individual built-in tools directly from the command line - useful for
/// scripting, debugging tool behaviour, or quick one-off operations without
/// starting an agent session.
///
/// Examples:
///
///   sven tool list
///   sven tool call read_file path=src/main.rs
///   sven tool call grep pattern=TODO path=./src include="*.rs"
///   sven tool call shell command="git status"
///   sven tool call grep --help
#[derive(Subcommand, Debug)]
pub enum ToolCommands {
    /// List all available built-in tools with their names and descriptions.
    ///
    /// Example:
    ///
    ///   sven tool list
    List,

    /// Call a built-in tool directly.
    ///
    /// With no arguments, or with only `--help`, prints all tools and their
    /// complete parameter schemas.
    ///
    /// With a tool name as the first argument and no further arguments, prints
    /// that tool's parameter schema.  Add key=value pairs to execute the tool.
    ///
    /// Parameter forms:
    ///   key=value            - string, bool (true/false), or integer
    ///   --json '{"k":"v"}'   - raw JSON object (overrides key=value pairs)
    ///
    /// Examples:
    ///
    ///   sven tool call                                 - list all tools + schemas
    ///   sven tool call --help                          - same
    ///   sven tool call grep                            - show grep's schema
    ///   sven tool call grep --help                     - show grep's schema
    ///   sven tool call grep pattern=TODO path=./src
    ///   sven tool call shell command="git status"
    ///   sven tool call write_file path=out.txt content="hello"
    ///   sven tool call shell --json '{"command":"ls -la"}'
    // disable_help_flag so --help lands in `args` and we can show tool-specific docs
    #[command(disable_help_flag = true)]
    Call {
        /// Everything after `call`:
        ///   (no args)              → print all tools + schemas
        ///   --help / -h            → same
        ///   <TOOL>                 → print that tool's schema
        ///   <TOOL> --help          → same
        ///   <TOOL> key=value ...   → execute the tool
        ///   <TOOL> --json '{...}'  → execute with raw JSON args
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

