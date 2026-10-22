// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven config`: read and write one key of the configuration files.

use clap::Subcommand;
use sven_config::scope::Scope;

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    /// Print the value a key has: the one in force (written in a file, or
    /// the default), or with --scope what that file says.
    ///
    ///   sven config get model.name
    ///   sven config get --scope user agent.max_tool_rounds
    Get {
        /// The key, as a dotted path (`model.name`); a segment holding a dot
        /// is quoted (`providers.or.models."z-ai/glm-5.3".max_tokens`).
        key: String,
        /// Read only this file: `project` (the configuration file here),
        /// `workspace` (the nearest one above here) or `user`.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<Scope>,
    },
    /// Write one key into a configuration file, creating it as needed. The
    /// file holds only what was written into it; every other key keeps its
    /// default. A key sven does not know, or a value of the wrong type, is
    /// refused and the file left as it was.
    ///
    ///   sven config set model.name z-ai/glm-5.3-flash
    ///   sven config set --scope user model.temperature 0.3
    ///   sven config set model.api_key --stdin
    Set {
        /// The key, as a dotted path (`model.name`); a segment holding a dot
        /// is quoted.
        key: String,
        /// The value, read as YAML: `0.3` is a number, `true` a bool,
        /// `[a, b]` a list, anything else text.
        #[arg(required_unless_present = "stdin", conflicts_with = "stdin")]
        value: Option<String>,
        /// Read the value from stdin (typed without echo on a terminal), so a
        /// key never reaches the shell's history.
        #[arg(long)]
        stdin: bool,
        /// The file to write: `project` (the configuration file here, made as
        /// `.sven.yaml`; the default), `workspace` (the nearest one above
        /// here) or `user`.
        #[arg(long, value_name = "SCOPE", default_value = "project")]
        scope: Scope,
    },
}
