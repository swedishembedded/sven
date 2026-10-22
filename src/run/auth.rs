// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven auth`: store, list and forget hosted providers' keys
//! (`sven_model_drivers::credentials`).

use std::io::{BufRead, IsTerminal, Write};

use crate::cli::AuthCommands;

pub(crate) fn run_auth_command(command: &AuthCommands) -> anyhow::Result<()> {
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
        }
        AuthCommands::Remove { provider } => {
            if credentials::remove(provider)? {
                println!("forgot the {provider} key");
            } else {
                println!("no key was stored for {provider}");
            }
        }
    }
    Ok(())
}

/// One line from stdin; on a terminal, prompted on stderr and not echoed.
fn read_secret(prompt: &str) -> anyhow::Result<String> {
    let stdin = std::io::stdin();
    let interactive = stdin.is_terminal();
    if interactive {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
    }
    let _echo = interactive.then(EchoOff::new).flatten();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    if interactive {
        eprintln!();
    }
    Ok(line)
}

/// Terminal echo turned off for as long as it lives.
struct EchoOff {
    #[cfg(unix)]
    saved: libc::termios,
}

impl EchoOff {
    #[cfg(unix)]
    fn new() -> Option<Self> {
        // SAFETY: tcgetattr/tcsetattr on stdin with a zeroed termios that
        // tcgetattr fills before it is read.
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut saved) != 0 {
                return None;
            }
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            (libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) == 0)
                .then_some(Self { saved })
        }
    }

    #[cfg(not(unix))]
    fn new() -> Option<Self> {
        None
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: restores the attributes tcgetattr returned.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
    }
}
