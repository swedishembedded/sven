// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven auth`: store, list and forget hosted providers' keys
//! (`sven_model_drivers::credentials`), and sign in to a remote-control
//! server (`sven_remote::login`).

use crate::cli::AuthCommands;
use crate::run::secret::read_secret;

pub(crate) async fn run_auth_command(command: &AuthCommands) -> anyhow::Result<()> {
    use sven_model_drivers::credentials;
    match command {
        AuthCommands::Set { provider, key_file } => {
            let Some(meta) = sven_model::get_driver(provider) else {
                anyhow::bail!("unknown provider {provider:?}; `sven list-providers` lists them");
            };
            anyhow::ensure!(
                provider != "brain",
                "brain needs no stored key: sven reads it from the connection record a running `brain serve` publishes"
            );
            anyhow::ensure!(
                meta.default_base_url.is_some(),
                "{provider} has no endpoint of its own (its base_url is configured), so its key is configured with it: \
                 give `api_key_file` beside the `base_url`"
            );
            if !meta.requires_api_key {
                eprintln!("note: {provider} does not require a key; storing it anyway");
            }
            let key = match key_file {
                Some(file) => std::fs::read_to_string(file)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", file.display()))?,
                None => read_secret(&format!("{} key: ", meta.name))?,
            };
            let file = credentials::set(provider, &key)?;
            println!(
                "stored the {provider} key in {}; `{provider}/<model>` now uses it",
                file.display()
            );
        }
        AuthCommands::List => {
            let stored = credentials::providers();
            if stored.is_empty() {
                println!("no stored keys; `sven auth set <provider>` stores one");
            }
            for provider in stored {
                println!("{provider}");
            }
            #[cfg(feature = "network")]
            if let Some(login) = sven_remote::login::load()? {
                println!(
                    "remote control: {} as \"{}\" for {}",
                    login.server, login.workspace_name, login.account
                );
            }
        }
        AuthCommands::Remove { provider } => {
            if credentials::remove(provider)? {
                println!("forgot the {provider} key");
            } else {
                println!("no key was stored for {provider}");
            }
        }
        #[cfg(feature = "network")]
        AuthCommands::Login { server, name } => {
            let name = name
                .clone()
                .unwrap_or_else(sven_remote::login::machine_name);
            let login =
                sven_remote::login::sign_in(server, &name, |line| println!("{line}")).await?;
            println!(
                "Authentication successful. Signed in to {} as \"{}\" for {}. \
                 Run `sven rc` to take sessions.",
                login.server, login.workspace_name, login.account
            );
        }
        #[cfg(feature = "network")]
        AuthCommands::Logout => {
            if sven_remote::login::forget()? {
                println!("signed out of remote control");
            } else {
                println!("not signed in to remote control");
            }
        }
    }
    Ok(())
}
