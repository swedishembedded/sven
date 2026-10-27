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

/// `sven rc attach`: prints the conversation as it changes and sends each
/// line typed as its next message, until Ctrl-C or end of input.
pub(crate) async fn attach_remote_conversation(conversation: uuid::Uuid) -> anyhow::Result<()> {
    use sven_remote::transcript::{Reader, FOLLOW_INTERVAL};
    use tokio::io::AsyncBufReadExt as _;

    let login = signed_in()?;
    let client = sven_remote::RemoteClient::new(&login.control_url, &login.token)?;
    let mut reader = Reader::new(conversation);
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let mut poll = tokio::time::interval(FOLLOW_INTERVAL);
    println!("attached; type a message and press Enter, Ctrl-C to detach");
    loop {
        tokio::select! {
            _ = poll.tick() => {
                for line in reader.read(&client).await? {
                    println!("{line}");
                }
            }
            line = lines.next_line() => {
                let Some(text) = line? else { return Ok(()) };
                if text.trim().is_empty() {
                    continue;
                }
                match client.send_to_conversation(conversation, &text).await {
                    Ok(()) => {}
                    Err(e) if e.downcast_ref::<sven_remote::Busy>().is_some() => {
                        println!("({e}; send it when the answer is done)");
                    }
                    Err(e) => return Err(e),
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// The agent the full-screen UI talks to for `sven rc attach --tui`: the
/// conversation as the server keeps it (`sven_remote::attach`).
#[cfg(feature = "tui")]
pub(crate) fn attached_agent(conversation: uuid::Uuid) -> anyhow::Result<sven_tui::AgentTask> {
    let login = signed_in()?;
    let client = sven_remote::RemoteClient::new(&login.control_url, &login.token)?;
    Ok(Box::new(move |requests, events| {
        Box::pin(sven_remote::attach::remote_agent_task(
            client,
            conversation,
            requests,
            events,
        ))
    }))
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
