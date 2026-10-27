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
    Ok(Box::new(move |requests, events, questions| {
        Box::pin(sven_remote::attach::remote_agent_task(
            client,
            conversation,
            requests,
            events,
            questions,
        ))
    }))
}

/// A hosted host, confined and ready to run: what it read before the
/// confinement, and its sandbox.
pub(crate) struct HostedStart {
    config: Config,
    sandbox: PathBuf,
}

/// For `sven rc --hosted --sandbox DIR`: reads the configuration, then
/// confines this process to `DIR` (`sven_remote::sandbox`). Must run before
/// any thread is started. None for every other command.
pub(crate) fn confine_hosted(cli: &crate::cli::Cli) -> anyhow::Result<Option<HostedStart>> {
    let Some(crate::cli::Commands::Rc(args)) = &cli.command else {
        return Ok(None);
    };
    let (true, Some(sandbox)) = (args.hosted, &args.sandbox) else {
        return Ok(None);
    };
    let sandbox = sandbox
        .canonicalize()
        .with_context(|| format!("the sandbox {}", sandbox.display()))?;
    let mut config = sven_frontend::Settings::load(cli.config.as_deref())?.runtime;
    // An allow list, not a deny list: a tool added later is never offered
    // to a hosted session unless named in HOSTED_TOOLS.
    config.tools.only = Some(
        sven_remote::HOSTED_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
    );
    sven_remote::sandbox::confine(&sandbox)?;
    Ok(Some(HostedStart { config, sandbox }))
}

/// Runs a hosted host confined by [`confine_hosted`]: everything it keeps is
/// in its sandbox.
pub(crate) async fn run_hosted_host(start: HostedStart) -> anyhow::Result<()> {
    let HostedStart { config, sandbox } = start;
    let login = sven_remote::login::load_from(&sandbox.join("remote.yaml"))?.ok_or_else(|| {
        anyhow::anyhow!(
            "no sign-in in {}; the server that starts a hosted host writes it",
            sandbox.join("remote.yaml").display()
        )
    })?;
    let working_dir = sandbox.join("work");
    std::fs::create_dir_all(&working_dir).with_context(|| format!("{}", working_dir.display()))?;
    sven_remote::run_host(sven_remote::HostOptions {
        config: Arc::new(config),
        working_dir,
        login,
        sessions: sven_remote::SessionStore::new(sandbox.join("sessions")),
    })
    .await
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
