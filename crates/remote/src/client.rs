// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The HTTP side of [`sven_control::remote`].

use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use futures::{Stream, StreamExt};
use sven_control::remote::{
    control_endpoint, conversation_approvals_endpoint, conversation_doc_endpoint,
    conversation_messages_endpoint, AttachRequest, Attached, CommandEnvelope, EventBatch,
    RemoteDiscovery, WhoAmI, DISCOVERY_PATH, REMOTE_PROTOCOL_VERSION,
};
use uuid::Uuid;

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

/// Another live `sven rc` holds the environment of this directory, or took
/// it over from this one, which must stop; the reason is the server's.
#[derive(Debug)]
pub struct EnvironmentTaken(pub String);

impl std::fmt::Display for EnvironmentTaken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EnvironmentTaken {}

/// The server's reason for a 409 on a host's request: another run holds, or
/// took over, the environment.
async fn superseded(response: reqwest::Response) -> EnvironmentTaken {
    EnvironmentTaken(
        response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| body.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| "another sven rc holds this environment".into()),
    )
}

/// The server knows no conversation by this id that runs on this host.
#[derive(Debug)]
pub struct ConversationNotFound(pub Uuid);

impl std::fmt::Display for ConversationNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no conversation {} runs on this machine", self.0)
    }
}

impl std::error::Error for ConversationNotFound {}

/// The server did not take the message now (an answer is being written);
/// the reason is the server's.
#[derive(Debug)]
pub struct Busy(pub String);

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Busy {}

/// The machine the conversation runs on is not connected, so the server
/// stored nothing; the reason says where to run `sven rc` to reconnect it.
#[derive(Debug)]
pub struct Disconnected(pub String);

impl std::fmt::Display for Disconnected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Disconnected {}

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

/// A command the server sent that this host cannot read (one of a newer
/// protocol, or malformed). Reconnecting resends it from the same place, so
/// a host that stops at it never moves on: it skips it by its `seq`.
#[derive(Debug)]
pub struct UnreadableCommand {
    /// The command's position in the stream, when the payload still says.
    pub seq: Option<u64>,
    reason: String,
}

impl std::fmt::Display for UnreadableCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "an unreadable command ({})", self.reason)
    }
}

impl std::error::Error for UnreadableCommand {}

fn decode_command(data: &str) -> anyhow::Result<CommandEnvelope> {
    serde_json::from_str(data).map_err(|e| {
        let seq = serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|value| value.get("seq")?.as_u64());
        UnreadableCommand {
            seq,
            reason: e.to_string(),
        }
        .into()
    })
}

/// A host's connection to its server.
#[derive(Clone)]
pub struct RemoteClient {
    http: reqwest::Client,
    control_url: String,
    token: String,
    /// The environment this host attached to and the run it attached with;
    /// both named on its command stream and uploads, which the server
    /// refuses once another run holds the environment.
    attached: Option<(Uuid, Uuid)>,
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
            attached: None,
        })
    }

    /// Attaches this host to the environment of `path` for `run`, and from
    /// then on names it on the command stream and uploads.
    ///
    /// # Errors
    ///
    /// The token is refused, another live host holds the environment
    /// ([`EnvironmentTaken`]), or the server cannot be asked.
    pub async fn attach_environment(&mut self, path: &str, run: Uuid) -> anyhow::Result<Uuid> {
        let response = self
            .http
            .post(self.url("environment"))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&AttachRequest {
                path: path.to_string(),
                run,
            })
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            409 => return Err(superseded(response).await.into()),
            _ => {}
        }
        anyhow::ensure!(
            response.status().is_success(),
            "attaching the environment answered {}",
            response.status()
        );
        let attached: Attached = response.json().await?;
        self.attached = Some((attached.environment_id, run));
        Ok(attached.environment_id)
    }

    /// Lets the environment go as this host stops, so `sven rc` started
    /// there again attaches at once. Best effort: a host that cannot say so
    /// is let go once it has been silent for a while.
    ///
    /// # Errors
    ///
    /// The server could not be told.
    pub async fn detach_environment(&self) -> anyhow::Result<()> {
        if self.attached.is_none() {
            return Ok(());
        }
        let response = self
            .http
            .delete(self.url("environment"))
            .query(&self.environment_query())
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        anyhow::ensure!(
            response.status().is_success(),
            "detaching the environment answered {}",
            response.status()
        );
        Ok(())
    }

    /// `?environment=&run=` for the host's own requests, once attached.
    fn environment_query(&self) -> Vec<(&'static str, String)> {
        self.attached
            .map(|(environment, run)| {
                vec![
                    ("environment", environment.to_string()),
                    ("run", run.to_string()),
                ]
            })
            .unwrap_or_default()
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
            .query(&self.environment_query())
            .bearer_auth(&self.token)
            .header("accept", "text/event-stream")
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            409 => return Err(superseded(response).await.into()),
            _ => {}
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
                        .map(|data| decode_command(&data))
                        .collect(),
                },
            };
            futures::stream::iter(parsed)
        }))
    }

    /// The update that brings a replica of conversation `id`'s shared
    /// document, whose state vector is `state_vector`, up to the server's.
    ///
    /// # Errors
    ///
    /// The token is refused, the conversation is not one that runs on this
    /// host ([`ConversationNotFound`]), or the server cannot be asked.
    pub async fn sync_conversation(
        &self,
        id: Uuid,
        state_vector: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        let response = self
            .http
            .get(self.url(&conversation_doc_endpoint(id)))
            .query(&[("sv", URL_SAFE_NO_PAD.encode(state_vector))])
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            404 => return Err(ConversationNotFound(id).into()),
            _ => {}
        }
        anyhow::ensure!(
            response.status().is_success(),
            "the conversation's document answered {}",
            response.status()
        );
        Ok(response.bytes().await?.to_vec())
    }

    /// Adds `text` to conversation `id` as the person's next message; the
    /// server then asks this host to answer it.
    ///
    /// # Errors
    ///
    /// The token is refused, the conversation does not run on this host
    /// ([`ConversationNotFound`]), it is answering already ([`Busy`]), the
    /// machine it runs on is not connected ([`Disconnected`]), or
    /// the server cannot be asked.
    pub async fn send_to_conversation(&self, id: Uuid, text: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .post(self.url(&conversation_messages_endpoint(id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => return Err(TokenRefused.into()),
            404 => return Err(ConversationNotFound(id).into()),
            409 => {
                let body = response.json::<serde_json::Value>().await.ok();
                let field = |name: &str| {
                    body.as_ref()
                        .and_then(|body| body.get(name)?.as_str().map(str::to_string))
                };
                if field("code").as_deref() == Some("environment_disconnected") {
                    let reason =
                        field("error").unwrap_or_else(|| "the machine is not connected".into());
                    return Err(Disconnected(reason).into());
                }
                let reason = field("error").unwrap_or_else(|| "the conversation is busy".into());
                return Err(Busy(reason).into());
            }
            _ => {}
        }
        anyhow::ensure!(
            response.status().is_success(),
            "sending the message answered {}",
            response.status()
        );
        Ok(())
    }

    /// Allows (`approve`) or refuses tool call `call_id` waiting in
    /// conversation `id`. False when nothing waits for that decision any
    /// more (it was decided elsewhere, or the answer ended).
    ///
    /// # Errors
    ///
    /// The token is refused, or the server cannot be asked.
    pub async fn decide_approval(
        &self,
        id: Uuid,
        call_id: &str,
        approve: bool,
    ) -> anyhow::Result<bool> {
        let response = self
            .http
            .post(self.url(&conversation_approvals_endpoint(id)))
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(&serde_json::json!({ "callId": call_id, "approve": approve }))
            .send()
            .await?;
        match response.status().as_u16() {
            401 | 403 => Err(TokenRefused.into()),
            404 => Ok(false),
            _ if response.status().is_success() => Ok(true),
            _ => anyhow::bail!("deciding the call answered {}", response.status()),
        }
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
            .query(&self.environment_query())
            .bearer_auth(&self.token)
            .timeout(REQUEST_TIMEOUT)
            .json(batch)
            .send()
            .await?;
        if response.status().as_u16() == 409 {
            return Err(superseded(response).await.into());
        }
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
    }

    #[test]
    fn an_unreadable_command_still_says_where_it_was() {
        let poison =
            decode_command(r#"{"seq":7,"command":{"type":"from_the_future"}}"#).unwrap_err();
        assert_eq!(
            poison.downcast_ref::<UnreadableCommand>().unwrap().seq,
            Some(7)
        );
        let garbage = decode_command("{").unwrap_err();
        assert_eq!(
            garbage.downcast_ref::<UnreadableCommand>().unwrap().seq,
            None
        );
        assert!(decode_command(r#"{"seq":1,"command":{"type":"list_slash_commands"}}"#).is_ok());
    }

    #[test]
    fn plain_http_is_for_this_machine_only() {
        for local in [
            "http://localhost:3000",
            "http://127.0.0.1:3000",
            "http://127.1.2.3",
            "http://[::1]:3000",
            "[::1]:3000",
        ] {
            assert!(normalize_server(local).is_ok(), "{local}");
        }
        for remote in [
            "http://example.com",
            "http://192.168.1.5:3000",
            "http://localhost.example.com",
            "http://127.0.0.1.example.com",
            "http://localhost@evil.example",
        ] {
            assert!(normalize_server(remote).is_err(), "{remote}");
        }
    }

    #[test]
    fn only_the_servers_own_origin_is_the_servers() {
        assert!(same_origin("https://s.example", "https://s.example/api"));
        assert!(same_origin("https://s.example", "https://s.example:443/v1"));
        for other in [
            "http://s.example/api",
            "https://evil.example/api",
            "https://s.example:8443/api",
            "https://s.example.evil.example/api",
            "not a url",
        ] {
            assert!(!same_origin("https://s.example", other), "{other}");
        }
        assert!(normalize_server("  ").is_err());
    }
}
