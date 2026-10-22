// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven auth`: the keys stored for hosted providers.

use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum AuthCommands {
    /// Store the key of a hosted provider. It then serves every model of that
    /// provider (`<provider>/<model>`) and every `providers:` entry whose
    /// driver it is. The key is read from stdin (typed without echo on a
    /// terminal), or from `--key-file`, never from the command line.
    ///
    ///   sven auth set openrouter
    ///   sven auth set anthropic --key-file ~/keys/anthropic
    Set {
        /// The provider's driver id (`sven list-providers`).
        provider: String,
        /// Read the key from this file instead of stdin.
        #[arg(long)]
        key_file: Option<std::path::PathBuf>,
    },
    /// List the providers with a stored key (the keys are not shown).
    List,
    /// Forget a provider's stored key.
    Remove {
        /// The provider's driver id.
        provider: String,
    },
}
