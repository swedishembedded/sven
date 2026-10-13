// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for hung and silently stalled model
// connections for its clients. If your team needs expertise in resilient
// agent runtimes then you can procure our services by sending an email to
// info@swedishembedded.com.
//! A [`ModelProvider`] that stalls on purpose, to test the watchdogs that
//! guard a streamed model turn.
//!
//! Each [`StallMode`] reproduces one way a real server goes quiet: it never
//! answers the request, it answers and then says nothing, or it keeps the
//! connection alive with keepalives and never produces a token. The keepalive
//! shape is the one the OpenAI-compatible driver really emits for an SSE
//! comment line: an empty [`ResponseEvent::TextDelta`].

use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};

/// How a [`StallingMockProvider`] misbehaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallMode {
    /// `complete()` never returns: the server accepted the connection and
    /// never sent response headers.
    NeverReturns,
    /// `complete()` returns a stream that never yields anything.
    HeadersThenSilent,
    /// The stream yields an empty `TextDelta` (a keepalive) every `interval`
    /// and never a real token.
    KeepaliveOnly { interval: Duration },
    /// The healthy control: `deltas` real text chunks, one every `interval`,
    /// then `Done`.
    Progressing { interval: Duration, deltas: usize },
}

/// A provider that stalls according to its [`StallMode`].
#[derive(Debug, Clone)]
pub struct StallingMockProvider {
    mode: StallMode,
    host: Option<String>,
    lead_deltas: usize,
}

impl StallingMockProvider {
    #[must_use]
    pub fn new(mode: StallMode) -> Self {
        Self {
            mode,
            host: None,
            lead_deltas: 0,
        }
    }

    /// Reports `host` as the server this provider talks to, so the errors
    /// that name the endpoint can be checked.
    #[must_use]
    pub fn with_host(mut self, host: &str) -> Self {
        self.host = Some(host.to_string());
        self
    }

    /// Emits `deltas` real text chunks before a `HeadersThenSilent` or
    /// `KeepaliveOnly` stream goes quiet: a server that stalls mid-answer.
    #[must_use]
    pub fn after_real_deltas(mut self, deltas: usize) -> Self {
        self.lead_deltas = deltas;
        self
    }
}

fn text_delta(text: &str) -> anyhow::Result<ResponseEvent> {
    Ok(ResponseEvent::TextDelta(text.to_string()))
}

/// What follows the lead chunks of a stalled stream.
#[derive(Clone, Copy)]
enum Tail {
    /// Nothing, ever.
    Silence,
    /// An empty `TextDelta` every interval.
    Keepalive(Duration),
}

/// `lead` real chunks, then `tail` until the stream is dropped.
fn lead_then(lead: usize, tail: Tail) -> ResponseStream {
    Box::pin(stream::unfold(0usize, move |sent| async move {
        if sent < lead {
            return Some((text_delta("tok "), sent + 1));
        }
        match tail {
            Tail::Silence => std::future::pending().await,
            Tail::Keepalive(interval) => {
                tokio::time::sleep(interval).await;
                Some((text_delta(""), sent + 1))
            }
        }
    }))
}

/// `deltas` real chunks, one per `interval`, then `Done`.
fn progressing(interval: Duration, deltas: usize) -> ResponseStream {
    Box::pin(stream::unfold(0usize, move |sent| async move {
        match sent.cmp(&deltas) {
            std::cmp::Ordering::Less => {
                tokio::time::sleep(interval).await;
                Some((text_delta("tok "), sent + 1))
            }
            std::cmp::Ordering::Equal => Some((Ok(ResponseEvent::Done), sent + 1)),
            std::cmp::Ordering::Greater => None,
        }
    }))
}

#[async_trait]
impl ModelProvider for StallingMockProvider {
    fn name(&self) -> &str {
        "stalling-mock"
    }

    fn model_name(&self) -> &str {
        "stalling-model"
    }

    fn endpoint_host(&self) -> Option<String> {
        self.host.clone()
    }

    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        match self.mode {
            StallMode::NeverReturns => std::future::pending().await,
            StallMode::HeadersThenSilent => Ok(lead_then(self.lead_deltas, Tail::Silence)),
            StallMode::KeepaliveOnly { interval } => {
                Ok(lead_then(self.lead_deltas, Tail::Keepalive(interval)))
            }
            StallMode::Progressing { interval, deltas } => Ok(progressing(interval, deltas)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test(start_paused = true)]
    async fn keepalives_are_empty_text_deltas_after_the_lead_chunks() {
        let provider = StallingMockProvider::new(StallMode::KeepaliveOnly {
            interval: Duration::from_secs(10),
        })
        .after_real_deltas(1);
        let mut stream = provider
            .complete(CompletionRequest::default())
            .await
            .unwrap();
        let mut chunks = Vec::new();
        for _ in 0..3 {
            chunks.push(stream.next().await.unwrap().unwrap());
        }
        let texts: Vec<&str> = chunks
            .iter()
            .map(|chunk| match chunk {
                ResponseEvent::TextDelta(text) => text.as_str(),
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["tok ", "", ""]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_progressing_stream_ends_with_done() {
        let provider = StallingMockProvider::new(StallMode::Progressing {
            interval: Duration::from_secs(1),
            deltas: 2,
        });
        let events: Vec<_> = provider
            .complete(CompletionRequest::default())
            .await
            .unwrap()
            .map(Result::unwrap)
            .collect()
            .await;
        assert_eq!(events.len(), 3);
        assert!(matches!(events[2], ResponseEvent::Done));
    }
}
