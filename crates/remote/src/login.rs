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
    /// The token the person copied from the server. A secret.
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

/// Signs this host in to `server`: shows the person where to approve it,
/// takes the token they copy back through `read_token`, checks it with the
/// server and stores it.
///
/// `say` receives the lines to show the person.
///
/// # Errors
///
/// The server cannot be reached or is not a remote-control server, or it
/// does not accept the token.
pub async fn sign_in(
    server: &str,
    name: &str,
    mut say: impl FnMut(&str),
    read_token: impl FnOnce() -> anyhow::Result<String>,
) -> anyhow::Result<RemoteLogin> {
    let server = crate::client::normalize_server(server)?;
    let found = crate::client::discover(&server).await?;
    let mut link = reqwest::Url::parse(&found.authorize_url)
        .map_err(|e| anyhow::anyhow!("{server} gave an unusable approval address: {e}"))?;
    link.query_pairs_mut().append_pair("name", name);
    say("Open this address, approve the connection, and paste the token it shows:");
    say(&format!("  {link}"));
    let token = read_token()?.trim().to_string();
    anyhow::ensure!(!token.is_empty(), "no token was pasted");
    let who = crate::client::RemoteClient::new(&found.control_url, &token)?
        .whoami()
        .await?;
    let login = RemoteLogin {
        server,
        control_url: found.control_url,
        openai_base_url: found.openai_base_url,
        token,
        workspace_id: who.workspace_id,
        workspace_name: who.workspace_name,
        account: who.account,
    };
    save(&login)?;
    Ok(login)
}
