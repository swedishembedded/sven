// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven auth`: the keys stored for hosted providers.

use clap::Subcommand;

/// The server `sven auth login` signs in to when told no other.
#[cfg(feature = "network")]
pub const DEFAULT_SERVER: &str = "https://sven.swedishembedded.com";

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
    /// Sign this machine in to a remote-control server, so `sven rc` can run
    /// sessions for it. Prints an address and a code: open the address, sign in
    /// and enter the code to approve this machine; sven waits for it.
    ///
    ///   sven auth login
    ///   sven auth login --server localhost:3000
    ///   sven auth login --server example.com --name build-box
    #[cfg(feature = "network")]
    Login {
        /// The server's address; the hosted sven service by default. Without
        /// a scheme, `http://` is assumed for this machine and `https://` for
        /// any other.
        #[arg(long, default_value = DEFAULT_SERVER)]
        server: String,
        /// What the server calls this machine; its host name by default.
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget the remote-control sign-in. The server keeps the connection
    /// until it is removed there.
    #[cfg(feature = "network")]
    Logout,
}
