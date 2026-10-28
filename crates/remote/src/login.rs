// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The remote-control server this user signed in to.
//!
//! `sven auth login --server URL` writes `<config dir>/sven/remote.yaml`,
//! owner-only (`0600`) like the provider keys next to it. One server at a
//! time: signing in again replaces it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::device::{self, Pacing};
use uuid::Uuid;

/// A signed-in host: where its server is and the token that proves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteLogin {
    /// The server's origin, as the user named it (normalized).
    pub server: String,
    /// From the server's discovery document at sign-in.
    pub control_url: String,
    pub openai_base_url: String,
    /// The key the server minted when the person approved this host. A secret.
    pub token: String,
    /// What the server calls this host.
    pub workspace_id: Uuid,
    pub workspace_name: String,
    /// The account the host acts for.
    pub account: String,
}

/// The login file of this user.
#[must_use]
pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("sven").join("remote.yaml"))
}

/// The stored login, if this user signed in.
///
/// # Errors
///
/// The file exists but cannot be read or is not a login.
pub fn load() -> anyhow::Result<Option<RemoteLogin>> {
    let Some(file) = path() else { return Ok(None) };
    load_from(&file)
}

/// The login stored in `file`, if there is one.
///
/// # Errors
///
/// The file exists but cannot be read or is not a login.
pub fn load_from(file: &std::path::Path) -> anyhow::Result<Option<RemoteLogin>> {
    match std::fs::read_to_string(file) {
        Ok(text) => serde_yaml::from_str(&text)
            .map(Some)
            .map_err(|e| anyhow::anyhow!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", file.display())),
    }
}

/// The login in `text`, read from `origin` (named in the error).
///
/// # Errors
///
/// `text` is not a login.
pub fn parse(text: &str, origin: &str) -> anyhow::Result<RemoteLogin> {
    serde_yaml::from_str(text).map_err(|e| anyhow::anyhow!("{origin}: {e}"))
}

/// Stores `login`, replacing any other.
///
/// # Errors
///
/// No configuration directory, or the write failed.
pub fn save(login: &RemoteLogin) -> anyhow::Result<PathBuf> {
    let file = path().ok_or_else(|| anyhow::anyhow!("no configuration directory for this user"))?;
    sven_model_drivers::credentials::write_private(&file, &serde_yaml::to_string(login)?)?;
    Ok(file)
}

/// Forgets the stored login. Whether there was one.
///
/// # Errors
///
/// The file exists but could not be removed.
pub fn forget() -> anyhow::Result<bool> {
    let Some(file) = path() else { return Ok(false) };
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", file.display())),
    }
}

/// This machine's host name, as a default name for the host.
#[must_use]
pub fn machine_name() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length, and gethostname
    // writes at most that many bytes.
    let status = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    match std::str::from_utf8(&buffer[..end]) {
        Ok(name) if status == 0 && !name.is_empty() => name.to_string(),
        _ => "sven".to_string(),
    }
}

/// Signs this host in to `server` with the device grant: shows the person
/// the page and the code to type there, waits for the approval, checks the
/// token the server then hands over and stores it.
///
/// `say` receives the lines to show the person. Ctrl-C ends the wait.
///
/// # Errors
///
/// The server cannot be reached, is not a remote-control server or predates
/// the device grant; the person denied the connection or let the code
/// expire; or the server does not accept the token.
pub async fn sign_in(
    server: &str,
    name: &str,
    say: impl FnMut(&str),
) -> anyhow::Result<RemoteLogin> {
    let cancel = async {
        // If the signal cannot be listened for, never cancel.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    sign_in_with(server, name, say, Pacing::default(), cancel, |login| {
        save(login).map(drop)
    })
    .await
}

/// [`sign_in`] with its clock, interruption and storage supplied.
///
/// # Errors
///
/// As [`sign_in`], and `store` failed.
pub async fn sign_in_with(
    server: &str,
    name: &str,
    mut say: impl FnMut(&str),
    pacing: Pacing,
    cancel: impl std::future::Future<Output = ()>,
    store: impl FnOnce(&RemoteLogin) -> anyhow::Result<()>,
) -> anyhow::Result<RemoteLogin> {
    let server = crate::client::normalize_server(server)?;
    let found = crate::client::discover(&server).await?;
    let (Some(request_url), Some(token_url)) = (
        found.device_authorization_url.as_deref(),
        found.device_token_url.as_deref(),
    ) else {
        anyhow::bail!(
            "{server} does not offer device sign-in; the server must be updated before `sven auth login` can connect to it"
        );
    };
    let http = crate::client::http()?;
    let grant = device::request(&http, request_url, name).await?;
    say(&format!(
        "Open {} and enter the code",
        grant.verification_uri
    ));
    say("");
    say(&format!("    {}", grant.user_code));
    say("");
    say(&format!(
        "Waiting for approval (the code works for {} minutes; Ctrl-C cancels)...",
        grant.expires_in.div_ceil(60)
    ));
    let granted = device::await_token(&http, token_url, &grant, pacing, cancel).await?;
    let who = crate::client::RemoteClient::new(&found.control_url, &granted.token)?
        .whoami()
        .await?;
    let login = RemoteLogin {
        server,
        control_url: found.control_url,
        openai_base_url: found.openai_base_url,
        token: granted.token,
        workspace_id: who.workspace_id,
        workspace_name: who.workspace_name,
        account: who.account,
    };
    store(&login)?;
    Ok(login)
}

#[cfg(test)]
mod tests;
