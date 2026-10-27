// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven rc`: this machine's sessions, run for the remote-control server
//! `sven auth login` signed in to.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use sven_bootstrap::Config;

pub(crate) async fn run_remote_host(
    config: Config,
    working_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let login = sven_remote::login::load()?.ok_or_else(|| {
        anyhow::anyhow!(
            "not signed in to a remote-control server; run `sven auth login --server URL` first"
        )
    })?;
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
