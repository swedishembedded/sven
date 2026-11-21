// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Finding a server: its address normalised to an origin, its discovery
//! document read, and every URL it names held to that origin.

use std::time::Duration;

use sven_control::remote::{RemoteDiscovery, DISCOVERY_PATH, REMOTE_PROTOCOL_VERSION};

use super::REQUEST_TIMEOUT;

/// `server` as an origin: a scheme is added when missing (`http://` for
/// this machine, `https://` for any other), and a trailing slash dropped.
///
/// # Errors
///
/// `server` is empty or names a scheme other than http(s).
pub fn normalize_server(server: &str) -> anyhow::Result<String> {
    let server = server.trim().trim_end_matches('/');
    anyhow::ensure!(!server.is_empty(), "the server address is empty");
    anyhow::ensure!(
        !server.contains("://") || server.starts_with("http://") || server.starts_with("https://"),
        "the server must be an http or https address"
    );
    let with_scheme = if server.contains("://") {
        server.to_string()
    } else {
        let host = server.split(['/', ':']).next().unwrap_or_default();
        let local = matches!(host, "localhost" | "127.0.0.1") || server.starts_with("[::1]");
        format!("{}://{server}", if local { "http" } else { "https" })
    };
    let url = reqwest::Url::parse(&with_scheme)
        .map_err(|e| anyhow::anyhow!("{server} is not a server address: {e}"))?;
    anyhow::ensure!(
        url.scheme() == "https" || is_loopback(&url),
        "{server} would send the token and every conversation in the clear; \
         use an https address (http is only for this machine)"
    );
    Ok(with_scheme)
}

/// Whether `url` names this machine itself.
fn is_loopback(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Refuses unless every URL `found` names, and each of `also`, is on
/// `server`'s own origin: what is sent to them (`sent`, as the refusal says
/// it) must not reach a third party.
pub(crate) fn ensure_on_server(
    server: &str,
    found: &RemoteDiscovery,
    also: &[(&str, &str)],
    sent: &str,
) -> anyhow::Result<()> {
    let named = [
        ("control_url", found.control_url.as_str()),
        ("openai_base_url", found.openai_base_url.as_str()),
    ]
    .into_iter()
    .chain(found.mcp_url.as_deref().map(|url| ("mcp_url", url)))
    .chain(
        found
            .mcp_token_url
            .as_deref()
            .map(|url| ("mcp_token_url", url)),
    )
    .chain(also.iter().copied());
    for (what, url) in named {
        anyhow::ensure!(
            same_origin(server, url),
            "{server} names {url} as its {what}, which is not on the same origin as the \
             server; sven will not send {sent} there"
        );
    }
    Ok(())
}

/// Whether `url` is on the same origin (scheme, host and port) as `server`.
/// What a server's discovery document names for the token and the model key
/// to be sent to must be, or a document that was altered on the way (or a
/// server that was) could send them elsewhere.
pub(crate) fn same_origin(server: &str, url: &str) -> bool {
    match (reqwest::Url::parse(server), reqwest::Url::parse(url)) {
        (Ok(server), Ok(url)) => server.origin() == url.origin(),
        _ => false,
    }
}

pub(crate) fn http() -> anyhow::Result<reqwest::Client> {
    Ok(sven_http::client_builder()
        .user_agent(concat!("sven/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .build()?)
}

/// What `server` says about itself.
///
/// # Errors
///
/// It cannot be reached, answers something else, or speaks a newer protocol.
pub async fn discover(server: &str) -> anyhow::Result<RemoteDiscovery> {
    match discover_once(server).await {
        Ok(found) => Ok(found),
        Err(Discovery::Unreachable(e) | Discovery::Refused(e)) => Err(e),
    }
}

/// Why discovery did not answer: the server could not be reached or failed
/// (worth another try), or it answered something final.
pub(crate) enum Discovery {
    Unreachable(anyhow::Error),
    Refused(anyhow::Error),
}

/// [`discover`], telling a failure that may pass (no answer, a 5xx) from
/// one that will not.
pub(crate) async fn discover_once(server: &str) -> Result<RemoteDiscovery, Discovery> {
    let url = format!("{server}{DISCOVERY_PATH}");
    let response = http()
        .map_err(Discovery::Refused)?
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        // The whole chain: reqwest's own text ("error sending request")
        // hides why (refused, a failed lookup, a proxy's answer, TLS).
        .map_err(|e| {
            Discovery::Unreachable(anyhow::anyhow!(
                "could not reach {server}: {}",
                sven_http::describe(&e)
            ))
        })?;
    if response.status().is_server_error() {
        return Err(Discovery::Unreachable(anyhow::anyhow!(
            "{url} answered {}",
            response.status()
        )));
    }
    discovered(server, &url, response)
        .await
        .map_err(Discovery::Refused)
}

async fn discovered(
    server: &str,
    url: &str,
    response: reqwest::Response,
) -> anyhow::Result<RemoteDiscovery> {
    anyhow::ensure!(
        response.status().is_success(),
        "{server} is not a remote-control server ({url} answered {})",
        response.status()
    );
    let found: RemoteDiscovery = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("{url} did not answer a discovery document: {e}"))?;
    anyhow::ensure!(
        found.protocol_version <= REMOTE_PROTOCOL_VERSION,
        "{server} speaks remote-control protocol {}, this sven speaks {REMOTE_PROTOCOL_VERSION}; update sven",
        found.protocol_version
    );
    Ok(found)
}
