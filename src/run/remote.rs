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
    // One listener for the whole run: a Ctrl-C that arrives while a sync is
    // in flight is otherwise caught by the handler and seen by no one.
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        let lines = tokio::select! {
            lines = reader.read(&client) => lines?,
            _ = &mut interrupted => return Ok(()),
        };
        for line in lines {
            println!("{line}");
        }
        if !follow {
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
            _ = &mut interrupted => return Ok(()),
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
    // One listener for the whole run (see `show_remote_conversation`).
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
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
                    Err(e) if e.downcast_ref::<sven_remote::Disconnected>().is_some() => {
                        println!("({e})");
                    }
                    Err(e) => return Err(e),
                }
            }
            _ = &mut interrupted => return Ok(()),
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
    login: sven_remote::login::RemoteLogin,
    sandbox: PathBuf,
}

/// The most a hosted host reads of its sign-in.
const MAX_HOSTED_LOGIN: u64 = 64 * 1024;

/// For `sven rc --hosted --sandbox DIR`: reads the configuration and the
/// sign-in, then confines this process to `DIR` (`sven_remote::sandbox`).
/// Must run before any thread is started. None for every other command.
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
    sven_remote::restrict_to_hosted(&mut config);
    // The server that starts it writes the sign-in to its standard input,
    // so the key is never in a file anything it runs could read.
    let mut login = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(std::io::stdin(), MAX_HOSTED_LOGIN),
        &mut login,
    )
    .context("reading the sign-in from standard input")?;
    let login = sven_remote::login::parse(&login, "the sign-in on standard input")?;
    sven_remote::sandbox::confine(&sandbox)?;
    Ok(Some(HostedStart {
        config,
        login,
        sandbox,
    }))
}

/// Runs a hosted host confined by [`confine_hosted`]: everything it keeps is
/// in its sandbox.
pub(crate) async fn run_hosted_host(start: HostedStart) -> anyhow::Result<()> {
    let HostedStart {
        config,
        login,
        sandbox,
    } = start;
    let working_dir = sandbox.join("work");
    std::fs::create_dir_all(&working_dir).with_context(|| format!("{}", working_dir.display()))?;
    sven_remote::run_host(sven_remote::HostOptions {
        config: Arc::new(config),
        working_dir,
        login,
        sessions: sven_remote::SessionStore::new(sandbox.join("sessions")),
        hosted: true,
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
        hosted: false,
    })
    .await
}
