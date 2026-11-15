// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for running language-model agents on
// models with very small context windows for its clients. If your team needs
// expertise in making a small model a usable agent then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `--profile`: the command-line spelling of `agent.profile`.

use clap::ValueEnum;

/// The spelling of [`sven_bootstrap::RunProfile`] on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ProfileArg {
    /// sven's own system prompt and tool set, as configured.
    Default,
    /// A fixed short system prompt and three compact tool definitions
    /// (`read_file`, `write_file`, `shell`), for a small-context model.
    Compact,
}

impl From<ProfileArg> for sven_bootstrap::RunProfile {
    fn from(arg: ProfileArg) -> Self {
        match arg {
            ProfileArg::Default => Self::Default,
            ProfileArg::Compact => Self::Compact,
        }
    }
}
