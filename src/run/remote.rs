// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven rc`: this machine's sessions, run for the remote-control server
//! `sven auth login` signed in to.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use sven_bootstrap::Config;

fn signed_in() -> anyhow::Result<sven_remote::RemoteLogin> {
    sven_remote::login::load()?.ok_or_else(|| {
        anyhow::anyhow!(
            "not signed in to a remote-control server; run `sven auth login --server URL` first"
        )
    })
}

/// `sven rc show`: prints the conversation's finished messages, and with
/// `follow` keeps syncing and printing until Ctrl-C.
pub(crate) async fn show_remote_conversation(
    conversation: uuid::Uuid,
    follow: bool,
) -> anyhow::Result<()> {
    use sven_remote::transcript::{Reader, FOLLOW_INTERVAL};

    let login = signed_in()?;
    let client = sven_remote::RemoteClient::new(&login.control_url, &login.token)?;
    let mut reader = Reader::new(conversation);
    loop {
        for line in reader.read(&client).await? {
            println!("{line}");
        }
        if !follow {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

pub(crate) async fn run_remote_host(
    config: Config,
    working_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let login = signed_in()?;
    let working_dir = match working_dir {
        Some(dir) => dir,
        None => std::env::current_dir().context("the current directory")?,
    };
    let working_dir = working_dir.canonicalize().with_context(|| {
        format!(
            "{} is not a directory sessions can work in",
            working_dir.display()
        )
    })?;
    let sessions = sven_remote::sessions::default_dir()
        .ok_or_else(|| anyhow::anyhow!("no data directory for this user to keep sessions in"))?;
    sven_remote::run_host(sven_remote::HostOptions {
        config: Arc::new(config),
        working_dir,
        login,
        sessions: sven_remote::SessionStore::new(sessions),
    })
    .await
}
