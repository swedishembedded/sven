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
/// confinement, and where it runs.
pub(crate) struct HostedStart {
    config: Config,
    login: sven_remote::login::RemoteLogin,
    place: HostedPlace,
}

/// Where a hosted host runs.
enum HostedPlace {
    /// Confined by the kernel to its own directory (`--sandbox DIR`).
    Sandbox(PathBuf),
    /// Inside a sandbox container (`--container`), reaching out only through
    /// the server's egress socket, when one is given.
    Container {
        egress: Option<PathBuf>,
        state: PathBuf,
    },
}

/// The most a hosted host reads of its sign-in.
const MAX_HOSTED_LOGIN: u64 = 64 * 1024;

/// For `sven rc --hosted`: reads the configuration and the sign-in, then
/// either confines this process to `--sandbox DIR` (`sven_remote::sandbox`)
/// or, in a sandbox container (`--container`), points it and everything it
/// runs at the egress proxy. Must run before any thread is started. None for
/// every other command.
pub(crate) fn confine_hosted(cli: &crate::cli::Cli) -> anyhow::Result<Option<HostedStart>> {
    let Some(crate::cli::Commands::Rc(args)) = &cli.command else {
        return Ok(None);
    };
    if !args.hosted {
        return Ok(None);
    }
    // Only the file `--config` names: the host's home and working directory
    // are where a tenant's own files are, so no layer is looked for there.
    let mut config = sven_frontend::Settings::load_explicit(cli.config.as_deref())?.runtime;
    // The server that starts it writes the sign-in to its standard input,
    // so the key is never in a file anything it runs could read.
    let mut login = String::new();
    std::io::Read::read_to_string(
        &mut std::io::Read::take(std::io::stdin(), MAX_HOSTED_LOGIN),
        &mut login,
    )
    .context("reading the sign-in from standard input")?;
    let login = sven_remote::login::parse(&login, "the sign-in on standard input")?;
    let place = match (&args.sandbox, args.container) {
        (Some(sandbox), false) => {
            let sandbox = sandbox
                .canonicalize()
                .with_context(|| format!("the sandbox {}", sandbox.display()))?;
            sven_remote::restrict_to_hosted(&mut config);
            sven_remote::sandbox::confine(&sandbox, &login.server_ports()?)?;
            HostedPlace::Sandbox(sandbox)
        }
        (None, true) => {
            sven_remote::container::restrict_to_container(&mut config);
            if args.egress_socket.is_some() {
                let proxy = sven_remote::container::proxy_url();
                for name in [
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "ALL_PROXY",
                ] {
                    // SAFETY: no other thread exists yet (this runs before
                    // the runtime starts), so nothing reads the environment
                    // while it changes.
                    unsafe { std::env::set_var(name, &proxy) };
                }
                for name in ["NO_PROXY", "no_proxy"] {
                    // SAFETY: as above.
                    unsafe { std::env::remove_var(name) };
                }
            }
            let state = args
                .state
                .clone()
                .context("a hosted container host needs --state, where it keeps its sessions")?;
            HostedPlace::Container {
                egress: args.egress_socket.clone(),
                state,
            }
        }
        _ => anyhow::bail!("a hosted host runs either in --sandbox DIR or in a --container"),
    };
    Ok(Some(HostedStart {
        config,
        login,
        place,
    }))
}

/// Runs a hosted host prepared by [`confine_hosted`].
pub(crate) async fn run_hosted_host(start: HostedStart) -> anyhow::Result<()> {
    let HostedStart {
        config,
        login,
        place,
    } = start;
    let (working_dir, kept) = match place {
        HostedPlace::Sandbox(sandbox) => (sandbox.join("work"), sandbox.join("sessions")),
        HostedPlace::Container { egress, state } => {
            if let Some(socket) = egress {
                let listener = tokio::net::TcpListener::bind((
                    "127.0.0.1",
                    sven_remote::container::EGRESS_PORT,
                ))
                .await
                .context("listening for the egress relay")?;
                tokio::spawn(async move {
                    if let Err(e) = sven_remote::container::relay(listener, &socket).await {
                        tracing::error!("the egress relay stopped: {e:#}");
                    }
                });
            }
            (
                PathBuf::from(sven_remote::container::WORKSPACE),
                state.join("sessions"),
            )
        }
    };
    std::fs::create_dir_all(&working_dir).with_context(|| format!("{}", working_dir.display()))?;
    sven_remote::run_host(sven_remote::HostOptions {
        config: Arc::new(config),
        working_dir,
        login,
        sessions: sven_remote::SessionStore::new(kept),
        hosted: true,
        wide_access: false,
    })
    .await
}

pub(crate) async fn run_remote_host(
    config: Config,
    working_dir: Option<PathBuf>,
    wide_access: bool,
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
        wide_access,
    })
    .await
}
