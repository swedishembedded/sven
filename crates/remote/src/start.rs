// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Signing a host in from a single-use start credential (`sven rc
//! --login-file`).
//!
//! A server that starts a container for a person hands the container a start
//! credential instead of a device grant nobody is present to approve. The host
//! trades it once for its own token. The result lives in memory only: nothing
//! here writes a sign-in to disk.
//!
//! Swedish Embedded AB implements solutions for credential handover to
//! unattended workloads for its clients. If your team needs expertise in
//! secure container bootstrapping then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use uuid::Uuid;

use crate::login::RemoteLogin;

/// The most a login file may hold.
pub const MAX_LOGIN_FILE: u64 = 64 * 1024;

/// What `--login-file` names: the server and the credential to start with.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginFile {
    pub server: String,
    /// Opaque, single use. A secret: never shown, never logged.
    start_credential: String,
}

impl std::fmt::Debug for LoginFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginFile")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

impl LoginFile {
    /// A login file's content, for callers that already hold it.
    #[must_use]
    pub fn new(server: impl Into<String>, start_credential: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            start_credential: start_credential.into(),
        }
    }
}

/// Reads the login file at `path` once. The file may be on a read-only
/// volume, so it is left as it is.
///
/// # Errors
///
/// The file cannot be read, exceeds [`MAX_LOGIN_FILE`] bytes, or is not a
/// login file.
pub fn read_login_file(path: &Path) -> anyhow::Result<LoginFile> {
    use std::io::Read as _;
    let shown = path.display();
    let file =
        std::fs::File::open(path).map_err(|e| anyhow::anyhow!("the login file {shown}: {e}"))?;
    let mut text = String::new();
    file.take(MAX_LOGIN_FILE + 1)
        .read_to_string(&mut text)
        .map_err(|e| anyhow::anyhow!("the login file {shown}: {e}"))?;
    anyhow::ensure!(
        text.len() as u64 <= MAX_LOGIN_FILE,
        "the login file {shown} is larger than {} KiB",
        MAX_LOGIN_FILE / 1024
    );
    // The parse error names a line and a field, never the credential.
    let file: LoginFile =
        serde_yaml::from_str(&text).map_err(|e| anyhow::anyhow!("the login file {shown}: {e}"))?;
    anyhow::ensure!(
        !file.start_credential.trim().is_empty(),
        "the login file {shown} has an empty start_credential"
    );
    Ok(file)
}

/// How long to wait between attempts, and how long to try in all.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    /// The wait after the first failed attempt; doubled after each.
    pub first: Duration,
    /// The longest single wait.
    pub longest: Duration,
    /// Give up when the waits so far reach this.
    pub total: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(1),
            longest: Duration::from_secs(16),
            total: Duration::from_secs(120),
        }
    }
}

/// The server's answer to a successful start. Unknown fields are ignored.
#[derive(Deserialize)]
struct Started {
    token: String,
    workspace_id: Uuid,
    workspace_name: String,
    account: String,
}

/// The server does not accept the start credential: wrong, already used or
/// expired. Retrying cannot help.
#[derive(Debug)]
pub struct StartCredentialRefused;

impl std::fmt::Display for StartCredentialRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "start credential refused: the server says it is invalid, already used or expired; \
             a start credential works once, so this host must be started again with a new one",
        )
    }
}

impl std::error::Error for StartCredentialRefused {}

/// Trades the start credential in `file` for a sign-in, held in memory.
/// Ctrl-C ends the wait.
///
/// # Errors
///
/// As [`exchange_start_credential_with`].
pub async fn exchange_start_credential(file: &LoginFile) -> anyhow::Result<RemoteLogin> {
    let cancel = async {
        // If the signal cannot be listened for, never cancel.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    exchange_start_credential_with(file, Backoff::default(), cancel).await
}

/// [`exchange_start_credential`] with its pacing and interruption supplied.
///
/// # Errors
///
/// The server cannot be reached for the whole `backoff`, is not a
/// remote-control server, or names its control or model API on another
/// origin (checked before the credential is sent); the credential is refused
/// ([`StartCredentialRefused`]); the server answers something unexpected; or
/// `cancel` completed first.
pub async fn exchange_start_credential_with(
    file: &LoginFile,
    backoff: Backoff,
    cancel: impl std::future::Future<Output = ()>,
) -> anyhow::Result<RemoteLogin> {
    let server = crate::client::normalize_server(&file.server)?;
    let found = crate::client::discover(&server).await?;
    // The credential goes where this names, and the token minted for it
    // after.
    for (what, url) in [
        ("control_url", &found.control_url),
        ("openai_base_url", &found.openai_base_url),
    ]
    .into_iter()
    .chain(found.mcp_url.as_ref().map(|url| ("mcp_url", url)))
    .chain(
        found
            .mcp_token_url
            .as_ref()
            .map(|url| ("mcp_token_url", url)),
    ) {
        anyhow::ensure!(
            crate::client::same_origin(&server, url),
            "{server} names {url} as its {what}, which is not on the same origin as the \
             server; sven will not send the start credential there"
        );
    }
    let http = crate::client::http()?;
    let url = format!("{}/start", found.control_url.trim_end_matches('/'));
    let started = tokio::select! {
        started = start_with_retries(&http, &url, &file.start_credential, backoff) => started?,
        () = cancel => anyhow::bail!("cancelled before the server answered"),
    };
    Ok(RemoteLogin {
        server,
        control_url: found.control_url,
        openai_base_url: found.openai_base_url,
        mcp_url: found.mcp_url,
        mcp_token_url: found.mcp_token_url,
        token: started.token,
        workspace_id: started.workspace_id,
        workspace_name: started.workspace_name,
        account: started.account,
    })
}

/// One attempt's failure: worth another try, or final.
enum Attempt {
    Transient(String),
    Final(anyhow::Error),
}

async fn start_with_retries(
    http: &reqwest::Client,
    url: &str,
    credential: &str,
    backoff: Backoff,
) -> anyhow::Result<Started> {
    let mut wait = backoff.first;
    let mut waited = Duration::ZERO;
    loop {
        let reason = match attempt(http, url, credential).await {
            Ok(started) => return Ok(started),
            Err(Attempt::Final(e)) => return Err(e),
            Err(Attempt::Transient(reason)) => reason,
        };
        if waited >= backoff.total {
            anyhow::bail!("the server did not accept the start request in time: {reason}");
        }
        tracing::warn!("start request failed ({reason}); trying again in {wait:?}");
        tokio::time::sleep(wait).await;
        waited += wait;
        wait = (wait * 2).min(backoff.longest);
    }
}

async fn attempt(http: &reqwest::Client, url: &str, credential: &str) -> Result<Started, Attempt> {
    let response = http
        .post(url)
        .bearer_auth(credential)
        .json(&serde_json::json!({}))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        // The error names the URL, never the header.
        .map_err(|e| Attempt::Transient(format!("could not reach {url}: {e}")))?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Attempt::Final(StartCredentialRefused.into()));
    }
    if status.is_server_error() {
        return Err(Attempt::Transient(format!("{url} answered {status}")));
    }
    if !status.is_success() {
        return Err(Attempt::Final(anyhow::anyhow!(
            "{url} answered {status} to the start request"
        )));
    }
    response.json().await.map_err(|e| {
        Attempt::Final(anyhow::anyhow!(
            "{url} did not answer a start document: {e}"
        ))
    })
}

#[cfg(test)]
mod tests;
