// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The remote-control server this user signed in to.
//!
//! `sven auth login` writes `<config dir>/sven/remote.yaml`, owner-only
//! (`0600`) like the provider keys next to it. One server at a time: signing
//! in again replaces it. The `sven` model provider reads its server and key
//! from the same file (`sven_model_drivers::sven_service`), so a sign-in also
//! gives this machine that server's models.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use sven_control::remote::RemoteDiscovery;
use uuid::Uuid;

use crate::device::{self, Pacing};

mod browser;
mod masked_input;
mod pkce;

pub use browser::{StdTerminal, Terminal};

/// A signed-in host: where its server is and the token that proves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteLogin {
    /// The server's origin, as the user named it (normalized).
    pub server: String,
    /// From the server's discovery document at sign-in.
    pub control_url: String,
    pub openai_base_url: String,
    /// The MCP endpoint of the person's connected accounts, when the server
    /// offers one; sessions reach it as the `connectors` MCP server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_url: Option<String>,
    /// Where this host's key is traded for short-lived tokens of that
    /// endpoint, when the server asks for them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_token_url: Option<String>,
    /// The key the server minted when the person approved this host. A secret.
    pub token: String,
    /// What the server calls this host.
    pub workspace_id: Uuid,
    pub workspace_name: String,
    /// The account the host acts for.
    pub account: String,
}

impl RemoteLogin {
    /// The TCP ports this sign-in's server, control API and model API are
    /// reached on, for a host that may connect nowhere else.
    ///
    /// # Errors
    ///
    /// A URL that does not parse or names no port.
    pub fn server_ports(&self) -> anyhow::Result<Vec<u16>> {
        let mut ports = Vec::new();
        for url in [&self.server, &self.control_url, &self.openai_base_url]
            .into_iter()
            .chain(self.mcp_url.as_ref())
            .chain(self.mcp_token_url.as_ref())
        {
            let parsed = reqwest::Url::parse(url)
                .map_err(|e| anyhow::anyhow!("the sign-in's URL {url}: {e}"))?;
            let port = parsed
                .port_or_known_default()
                .ok_or_else(|| anyhow::anyhow!("the sign-in's URL {url} names no port"))?;
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
        Ok(ports)
    }

    /// Where this host's searches go: the server's API, with its key, so
    /// the server runs and bills them (`tools.web.search.provider` can still
    /// choose Brave). Lives only in this process, never in a config file.
    #[must_use]
    pub fn search_endpoint(&self) -> sven_tools_web::SvenEndpoint {
        sven_tools_web::SvenEndpoint {
            base_url: self.openai_base_url.clone(),
            api_key: self.token.clone(),
        }
    }
}

/// The login file of this user.
#[must_use]
pub fn path() -> Option<PathBuf> {
    sven_model_drivers::sven_service::path()
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
    sven_config::write_private(&file, &serde_yaml::to_string(login)?)?;
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
#[cfg(unix)]
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

/// The default name for the host where the host name is not read: `sven`
/// (`sven auth login --name` names it).
#[must_use]
#[cfg(not(unix))]
pub fn machine_name() -> String {
    "sven".to_string()
}

/// Signs this host in to `server` and stores the key the server mints.
///
/// A server that offers the authorization code grant signs the person in
/// through the browser: a one-time code bound to this
/// terminal returns by a loopback redirect, or is pasted when the browser
/// is on another machine. A server that predates it gets the device grant:
/// the person is shown a page and a short code to type there and approves,
/// while this host polls.
///
/// `say` receives the lines to show the person. Ctrl-C ends the wait.
/// `no_browser` shows a link and asks for the code pasted instead of opening
/// a browser here.
///
/// # Errors
///
/// The server cannot be reached or is not a remote-control server; the
/// person denied the connection or let the code expire; or the server does
/// not accept the token.
pub async fn sign_in(
    server: &str,
    name: &str,
    no_browser: bool,
    say: impl FnMut(&str),
) -> anyhow::Result<RemoteLogin> {
    let cancel = async {
        // If the signal cannot be listened for, never cancel.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    sign_in_with(
        server,
        name,
        say,
        &mut StdTerminal::new(no_browser),
        Pacing::default(),
        cancel,
        |login| save(login).map(drop),
    )
    .await
}

/// [`sign_in`] with its terminal, clock, interruption and storage supplied.
///
/// # Errors
///
/// As [`sign_in`], and `store` failed.
pub async fn sign_in_with(
    server: &str,
    name: &str,
    mut say: impl FnMut(&str),
    terminal: &mut dyn Terminal,
    pacing: Pacing,
    cancel: impl std::future::Future<Output = ()>,
    store: impl FnOnce(&RemoteLogin) -> anyhow::Result<()>,
) -> anyhow::Result<RemoteLogin> {
    let server = crate::client::normalize_server(server)?;
    let found = crate::client::discover(&server).await?;
    let http = crate::client::http()?;
    let granted = match Grant::of(&found) {
        Grant::AuthorizationCode(endpoints) => {
            // The code, the verifier and the model key go where this names.
            crate::client::ensure_on_server(
                &server,
                &found,
                &endpoints.named(),
                "your sign-in",
            )?;
            browser::authorize(
                &http,
                &browser::CodeGrant {
                    server: &server,
                    authorization_endpoint: endpoints.authorization_endpoint,
                    token_endpoint: endpoints.token_endpoint,
                    code_display_uri: endpoints.code_display_uri,
                },
                name,
                &mut say,
                terminal,
                cancel,
            )
            .await?
        }
        Grant::Device {
            request_url,
            token_url,
        } => {
            crate::client::ensure_on_server(
                &server,
                &found,
                &[
                    ("device_authorization_url", request_url),
                    ("device_token_url", token_url),
                ],
                "your sign-in",
            )?;
            device_sign_in(&http, request_url, token_url, name, &mut say, pacing, cancel).await?
        }
        Grant::None => anyhow::bail!(
            "{server} offers no sign-in this sven can use; the server must be updated before `sven auth login` can connect to it"
        ),
    };
    let who = crate::client::RemoteClient::new(&found.control_url, &granted.token)?
        .whoami()
        .await?;
    let login = RemoteLogin {
        server,
        control_url: found.control_url,
        openai_base_url: found.openai_base_url,
        mcp_url: found.mcp_url,
        mcp_token_url: found.mcp_token_url,
        token: granted.token,
        workspace_id: who.workspace_id,
        workspace_name: who.workspace_name,
        account: who.account,
    };
    store(&login)?;
    Ok(login)
}

/// How a server lets a host sign in, newest first.
#[derive(Debug, PartialEq, Eq)]
enum Grant<'a> {
    AuthorizationCode(CodeEndpoints<'a>),
    Device {
        request_url: &'a str,
        token_url: &'a str,
    },
    None,
}

#[derive(Debug, PartialEq, Eq)]
struct CodeEndpoints<'a> {
    authorization_endpoint: &'a str,
    token_endpoint: &'a str,
    code_display_uri: Option<&'a str>,
}

impl<'a> CodeEndpoints<'a> {
    /// Every URL of the grant, by the name discovery gives it.
    fn named(&self) -> Vec<(&'static str, &'a str)> {
        [
            ("authorization_endpoint", Some(self.authorization_endpoint)),
            ("token_endpoint", Some(self.token_endpoint)),
            ("code_display_uri", self.code_display_uri),
        ]
        .into_iter()
        .filter_map(|(what, url)| url.map(|url| (what, url)))
        .collect()
    }
}

impl<'a> Grant<'a> {
    /// The code grant when the server advertises both its endpoints; the
    /// device grant only for a server that predates it.
    fn of(found: &'a RemoteDiscovery) -> Self {
        if let (Some(authorization_endpoint), Some(token_endpoint)) = (
            found.authorization_endpoint.as_deref(),
            found.token_endpoint.as_deref(),
        ) {
            return Self::AuthorizationCode(CodeEndpoints {
                authorization_endpoint,
                token_endpoint,
                code_display_uri: found.code_display_uri.as_deref(),
            });
        }
        match (
            found.device_authorization_url.as_deref(),
            found.device_token_url.as_deref(),
        ) {
            (Some(request_url), Some(token_url)) => Self::Device {
                request_url,
                token_url,
            },
            _ => Self::None,
        }
    }
}

/// The device grant: shows the page and the code to type there, waits for
/// the approval.
async fn device_sign_in(
    http: &reqwest::Client,
    request_url: &str,
    token_url: &str,
    name: &str,
    say: &mut impl FnMut(&str),
    pacing: Pacing,
    cancel: impl std::future::Future<Output = ()>,
) -> anyhow::Result<device::Granted> {
    let grant = device::request(http, request_url, name).await?;
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
    device::await_token(http, token_url, &grant, pacing, cancel).await
}

#[cfg(test)]
pub(crate) mod tests;
