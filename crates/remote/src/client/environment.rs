// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A host's own calls to its server: attaching to and letting go of the
//! environment of its directory, who its token belongs to, and the command
//! stream.

use std::time::Duration;

use futures::{Stream, StreamExt};
use sven_control::remote::{AttachRequest, Attached, CommandEnvelope, WhoAmI};
use uuid::Uuid;

use super::errors::{superseded, TokenRefused, UnreadableCommand};
use super::{RemoteClient, StreamLimits, COMMAND_STREAM, REQUEST_TIMEOUT};
use crate::sse::SseDecoder;

impl RemoteClient {
    /// Attaches this host to the environment of `path` for `run`, and from
    /// then on names it on the command stream and uploads.
    ///
    /// # Errors
    ///
    /// The token is refused, another live host holds the environment
    /// ([`EnvironmentTaken`](super::EnvironmentTaken)), or the server cannot be asked.
    pub async fn attach_environment(&mut self, path: &str, run: Uuid) -> anyhow::Result<Uuid> {
        let response = self
            .http
            .post(self.url("environment"))
            .bearer_auth(&self.token)
            // What this host honours: a turn marked `ask_every_tool` puts
            // every tool call to the server, so the server may send it one.
            .header(
                sven_control::HOST_CAPABILITIES_HEADER,
                sven_control::HOST_CAPABILITIES,
            )
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
    pub(super) fn environment_query(&self) -> Vec<(&'static str, String)> {
        self.attached
            .map(|(environment, run)| {
                vec![
                    ("environment", environment.to_string()),
                    ("run", run.to_string()),
                ]
            })
            .unwrap_or_default()
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
        self.commands_within(after, COMMAND_STREAM).await
    }

    /// [`Self::commands`], held to `limits`: a stream that does not open in
    /// time is an error, and one that falls silent ends with one.
    pub(crate) async fn commands_within(
        &self,
        after: u64,
        limits: StreamLimits,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<CommandEnvelope>> + Send + 'static> {
        let request = self
            .http
            .get(self.url("commands"))
            .query(&[("after", after)])
            .query(&self.environment_query())
            .bearer_auth(&self.token)
            .header("accept", "text/event-stream")
            .send();
        let response = tokio::time::timeout(limits.open, request)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "the command stream did not open within {} s",
                    limits.open.as_secs_f32()
                )
            })??;
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
        Ok(
            idle_bounded(response.bytes_stream(), limits.idle).flat_map(move |chunk| {
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
            }),
        )
    }
}

/// `chunks`, ended by an error once `idle` passes without one: any chunk,
/// a keepalive comment included, proves the connection is alive.
fn idle_bounded<B: Send + 'static>(
    chunks: impl Stream<Item = reqwest::Result<B>> + Send + 'static,
    idle: Duration,
) -> impl Stream<Item = anyhow::Result<B>> + Send + 'static {
    futures::stream::unfold(Some(Box::pin(chunks)), move |chunks| async move {
        let mut chunks = chunks?;
        match tokio::time::timeout(idle, chunks.next()).await {
            Ok(Some(chunk)) => Some((chunk.map_err(anyhow::Error::from), Some(chunks))),
            Ok(None) => None,
            Err(_) => Some((
                Err(anyhow::anyhow!(
                    "the command stream was silent for {} s",
                    idle.as_secs_f32()
                )),
                None,
            )),
        }
    })
}

pub(super) fn decode_command(data: &str) -> anyhow::Result<CommandEnvelope> {
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
