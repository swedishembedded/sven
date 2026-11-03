// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for signing command-line tools in
// to hosted services without a secret ever passing through a browser page, for
// its clients. If your team needs expertise in OAuth for native applications
// (RFC 7636, RFC 8252) then you can procure our services by sending an email
// to info@swedishembedded.com.
//! `sven auth login` by the authorization code grant with PKCE (RFC 7636)
//! over a loopback redirect (RFC 8252), or, where the browser cannot reach
//! this machine, over a page that shows the code to paste into the terminal.
//!
//! The code that comes back is single use and worthless without the verifier
//! only this process knows, so a code typed or pasted anywhere else cannot be
//! redeemed. A code is exchanged once, never retried.

use std::future::Future;
use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;

use super::pkce::{authorize_url, AuthorizeRequest, Pkce};
use crate::device::{ErrorBody, Granted, TokenBody};

/// How long one request to the token endpoint may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// What the person sees in the browser tab once sven has the code.
const SIGNED_IN_PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
<title>Sven - Signed in</title></head><body style=\"font-family: sans-serif; \
text-align: center; margin-top: 4rem\"><h1>Signed in.</h1>\
<p>You can close this tab and go back to your terminal.</p></body></html>";

/// The person's terminal and the browser on it: what a sign-in needs from
/// the machine, so that a test can play both.
#[async_trait(?Send)]
pub trait Terminal {
    /// Whether the browser cannot be expected to reach this machine's
    /// loopback port (a remote shell), or the person said not to open one.
    fn prefers_paste(&self) -> bool;
    /// Tries to open `url` in a browser; whether one was started.
    fn open_browser(&self, url: &str) -> bool;
    /// One line the person types or pastes, trimmed.
    async fn read_code(&mut self) -> anyhow::Result<String>;
}

/// This process's terminal and desktop.
pub struct StdTerminal {
    no_browser: bool,
}

impl StdTerminal {
    /// `no_browser`: never open one, show a link and ask for a pasted code.
    #[must_use]
    pub fn new(no_browser: bool) -> Self {
        Self { no_browser }
    }
}

/// Whether `lookup` says this shell runs over SSH, where a browser on this
/// machine (if any) is not the person's.
fn over_ssh(lookup: impl Fn(&str) -> Option<String>) -> bool {
    ["SSH_CONNECTION", "SSH_TTY"]
        .into_iter()
        .any(|name| lookup(name).is_some_and(|value| !value.is_empty()))
}

#[async_trait(?Send)]
impl Terminal for StdTerminal {
    fn prefers_paste(&self) -> bool {
        self.no_browser || over_ssh(|name| std::env::var(name).ok())
    }

    fn open_browser(&self, url: &str) -> bool {
        sven_mcp_client::open_browser(url)
    }

    async fn read_code(&mut self) -> anyhow::Result<String> {
        tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            let read = std::io::stdin().read_line(&mut line)?;
            anyhow::ensure!(read > 0, "no code was entered (end of input)");
            Ok(line.trim().to_string())
        })
        .await?
    }
}

/// The endpoints of a server that offers the code grant.
pub(super) struct CodeGrant<'a> {
    pub server: &'a str,
    pub authorization_endpoint: &'a str,
    pub token_endpoint: &'a str,
    pub code_display_uri: Option<&'a str>,
}

/// Where the code comes from.
enum Redirect {
    /// The browser returns to a port on this machine.
    Loopback(sven_mcp_client::Redirect),
    /// The server's page shows the code, to be pasted.
    Paste(String),
}

/// Signs `name` in at `grant`: sends the person to the browser, takes the
/// one-time code back (from the loopback port or pasted) and redeems it.
///
/// # Errors
///
/// The person denied the sign-in, never came back in time, or `cancel`
/// completed; the code is wrong or was used; the server cannot be reached; or
/// the server offers no way to hand a code to a terminal this machine's
/// browser cannot reach.
pub(super) async fn authorize(
    http: &reqwest::Client,
    grant: &CodeGrant<'_>,
    name: &str,
    say: &mut impl FnMut(&str),
    terminal: &mut dyn Terminal,
    cancel: impl Future<Output = ()>,
) -> anyhow::Result<Granted> {
    tokio::pin!(cancel);
    let pkce = Pkce::generate();
    let (redirect, url) = choose_redirect(grant, name, &pkce, terminal).await?;
    let (code, redirect_uri) = match redirect {
        Redirect::Loopback(listener) => {
            say(&sven_mcp_client::sign_in_link(grant.server, &url));
            say("");
            say("Waiting for the browser (Ctrl-C cancels)...");
            let query = tokio::select! {
                () = &mut cancel => anyhow::bail!("sign-in cancelled"),
                query = sven_mcp_client::wait_showing(
                    listener.listener,
                    &url,
                    &|_| {},
                    None,
                    sven_mcp_client::CALLBACK_TIMEOUT,
                    SIGNED_IN_PAGE,
                ) => query.map_err(|e| anyhow::anyhow!("sign-in failed: {e}"))?,
            };
            (code_of(&query)?, listener.redirect_uri)
        }
        Redirect::Paste(redirect_uri) => {
            say(&format!(
                "Open this link in a browser signed in to {}:",
                grant.server
            ));
            say("");
            say(&url);
            say("");
            say("Then paste the code it shows here and press Enter:");
            let code = tokio::select! {
                () = &mut cancel => anyhow::bail!("sign-in cancelled"),
                code = terminal.read_code() => code?,
            };
            anyhow::ensure!(!code.is_empty(), "no code was entered");
            (code, redirect_uri)
        }
    };
    let exchange = exchange_code(http, grant.token_endpoint, &code, &pkce, &redirect_uri);
    tokio::select! {
        () = &mut cancel => anyhow::bail!("sign-in cancelled"),
        granted = exchange => granted,
    }
}

/// Decides where the browser returns to and the address that sends it
/// there. A browser that cannot be opened here turns the sign-in into a
/// pasted code, when the server has a page for that.
async fn choose_redirect(
    grant: &CodeGrant<'_>,
    name: &str,
    pkce: &Pkce,
    terminal: &dyn Terminal,
) -> anyhow::Result<(Redirect, String)> {
    let url_for = |redirect_uri: &str| {
        authorize_url(
            grant.authorization_endpoint,
            &AuthorizeRequest {
                redirect_uri,
                state: &pkce.state,
                code_challenge: &pkce.challenge,
                name,
            },
        )
    };
    let paste = || -> anyhow::Result<(Redirect, String)> {
        let page = grant.code_display_uri.ok_or_else(|| {
            anyhow::anyhow!(
                "{} cannot hand a code to a terminal without a browser on this machine; \
                 run `sven auth login` where a browser opens",
                grant.server
            )
        })?;
        Ok((Redirect::Paste(page.to_string()), url_for(page)))
    };
    if terminal.prefers_paste() {
        return paste();
    }
    let listener = sven_mcp_client::bind_loopback().await?;
    let url = url_for(&listener.redirect_uri);
    if terminal.open_browser(&url) || grant.code_display_uri.is_none() {
        return Ok((Redirect::Loopback(listener), url));
    }
    paste()
}

/// The `code` of the query a callback arrived with.
fn code_of(query: &str) -> anyhow::Result<String> {
    reqwest::Url::parse(&format!("http://callback/?{query}"))?
        .query_pairs()
        .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        .filter(|code| !code.is_empty())
        .ok_or_else(|| anyhow::anyhow!("the browser came back without a code"))
}

/// Redeems `code` once at `token_endpoint`.
async fn exchange_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    code: &str,
    pkce: &Pkce,
    redirect_uri: &str,
) -> anyhow::Result<Granted> {
    let response = http
        .post(token_endpoint)
        .timeout(REQUEST_TIMEOUT)
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "code_verifier": pkce.verifier(),
            "redirect_uri": redirect_uri,
            "client_id": "sven",
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach the server to finish the sign-in: {e}"))?;
    let status = response.status();
    if status.is_success() {
        let body: TokenBody = response
            .json()
            .await
            .context("the server accepted the code but did not answer a token")?;
        return Ok(Granted {
            token: body.token,
            account: body.account,
        });
    }
    let body: Option<ErrorBody> = response.json().await.ok();
    Err(refusal(status, body))
}

/// What to tell the person about a refused code.
fn refusal(status: reqwest::StatusCode, body: Option<ErrorBody>) -> anyhow::Error {
    let Some(body) = body else {
        return anyhow::anyhow!(
            "the server answered {status} to the sign-in; run `sven auth login` again"
        );
    };
    if let Some(why) = body.error_description.filter(|why| !why.is_empty()) {
        return anyhow::anyhow!("sign-in refused: {why}");
    }
    match body.error.as_str() {
        "invalid_grant" => anyhow::anyhow!(
            "sign-in refused: the code is wrong, expired or already used; run `sven auth login` again"
        ),
        "access_denied" => anyhow::anyhow!(
            "sign-in refused: the connection was denied in the browser; run `sven auth login` again if that was a mistake"
        ),
        "invalid_request" => {
            anyhow::anyhow!("sign-in refused: the server rejected the request; update sven")
        }
        other => anyhow::anyhow!("sign-in refused ({other}); run `sven auth login` again"),
    }
}

#[cfg(test)]
mod tests;
