// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The HTTP side of [`sven_control::remote`].

use std::time::Duration;

use futures::{Stream, StreamExt};
use sven_control::remote::{
    control_endpoint, CommandEnvelope, EventBatch, RemoteDiscovery, WhoAmI, DISCOVERY_PATH,
    REMOTE_PROTOCOL_VERSION,
};

use crate::sse::SseDecoder;

/// The server no longer accepts the host's token: revoked, or never valid.
/// Retrying cannot help; the person signs in again.
#[derive(Debug)]
pub struct TokenRefused;

impl std::fmt::Display for TokenRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the server does not accept this host's token; sign in again with `sven auth login`",
        )
    }
}

impl std::error::Error for TokenRefused {}

/// How long a request that is not the command stream may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// `server` as an origin: a scheme is added when missing (`http://` for
/// this machine, `https://` for any other), and a trailing slash dropped.
///
/// # Errors
///
/// `server` is empty or names a scheme other than http(s).
pub fn normalize_server(server: &str) -> anyhow::Result<String> {
    let server = server.trim().trim_end_matches('/');
    anyhow::ensure!(!server.is_empty(), "the server address is empty");
    if server.starts_with("http://") || server.starts_with("https://") {
        return Ok(server.to_string());
    }
    anyhow::ensure!(
        !server.contains("://"),
        "the server must be an http or https address"
    );
    let host = server.split(['/', ':']).next().unwrap_or_default();
    let local = matches!(host, "localhost" | "127.0.0.1" | "[") || server.starts_with("[::1]");
    Ok(format!(
        "{}://{server}",
        if local { "http" } else { "https" }
    ))
}

fn http() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
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
    let url = format!("{server}{DISCOVERY_PATH}");
    let response = http()?
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach {server}: {e}"))?;
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

/// A host's connection to its server.
#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    control_url: String,
    token: String,
}

impl RemoteClient {
    /// # Errors
    ///
    /// The HTTP client cannot be built.
    pub fn new(control_url: &str, token: &str) -> anyhow::Result<Self> {
        Ok(Self {
            http: http()?,
            control_url: control_url.to_string(),
            token: token.to_string(),
        })
    }

    fn url(&self, endpoint: &str) -> String {
        control_endpoint(&self.control_url, endpoint)
    }

    /// Who the token belongs to.
    ///
    /// # Errors
    ///
    /// The token is refused (`401`/`403`), or the server cannot be asked.
    pub async fn whoami(&self) -> anyhow::Result<WhoAmI> {
        let response = self
            .http
            .get(self.url("whoami"))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(TokenRefused.into());
        }
        anyhow::ensure!(
            response.status().is_success(),
            "whoami answered {}",
            response.status()
        );
        Ok(response.json().await?)
    }

    /// The commands after `after`, as they arrive. Ends when the server closes
    /// the stream; the caller reconnects from the last `seq` it took.
    ///
    /// # Errors
    ///
    /// The stream cannot be opened, or the token is refused.
    pub async fn commands(
        &self,
        after: u64,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<CommandEnvelope>> + Send + 'static> {
        let response = self
            .http
            .get(self.url("commands"))
            .query(&[("after", after)])
            .bearer_auth(&self.token)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        if matches!(response.status().as_u16(), 401 | 403) {
            return Err(TokenRefused.into());
        }
        anyhow::ensure!(
            response.status().is_success(),
            "the command stream answered {}",
            response.status()
        );
        let mut decoder = SseDecoder::default();
        Ok(response.bytes_stream().flat_map(move |chunk| {
            let parsed: Vec<anyhow::Result<CommandEnvelope>> = match chunk {
                Err(e) => vec![Err(anyhow::anyhow!("the command stream broke: {e}"))],
                Ok(bytes) => match decoder.push(&bytes) {
                    Err(e) => vec![Err(e)],
                    Ok(payloads) => payloads
                        .into_iter()
                        .map(|data| {
                            serde_json::from_str(&data)
                                .map_err(|e| anyhow::anyhow!("an unreadable command ({e}): {data}"))
                        })
                        .collect(),
                },
            };
            futures::stream::iter(parsed)
        }))
    }

    /// Uploads `batch`.
    ///
    /// # Errors
    ///
    /// Not stored; the caller sends the same batch again.
    pub async fn post_events(&self, batch: &EventBatch) -> anyhow::Result<()> {
        let response = self
            .http
            .post(self.url("events"))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(batch)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "the event upload answered {}",
            response.status()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_address_gets_the_scheme_its_host_needs() {
        assert_eq!(
            normalize_server("localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_server("127.0.0.1:3000/").unwrap(),
            "http://127.0.0.1:3000"
        );
        assert_eq!(
            normalize_server("example.com").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_server("https://example.com/").unwrap(),
            "https://example.com"
        );
        assert!(normalize_server("ftp://example.com").is_err());
        assert!(normalize_server("  ").is_err());
    }
}
