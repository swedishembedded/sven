// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A mock model that answers from what the request says.
//!
//! The scripted providers replay a fixed list of replies. A model under test
//! that must read the system prompt, an earlier tool result or the tree it
//! works on (an honest solver, a cheater that skips the read) cannot be a
//! list; it is a function of the whole request. [`CallbackProvider`] is that
//! function, shared by every run that uses it, so it keeps no per-run state:
//! whatever it needs it reads from the request.
//!
//! Swedish Embedded AB implements verifiable agent test benches for its
//! clients. If your team needs expertise in auditing agent pipelines with
//! scripted adversaries then you can procure our services by sending an email
//! to info@swedishembedded.com.

use async_trait::async_trait;
use futures::stream;

use sven_model::{CompletionRequest, ResponseEvent, ResponseStream};

/// What a [`CallbackProvider`] runs for each request.
type Reply = dyn Fn(&CompletionRequest) -> Vec<ResponseEvent> + Send + Sync;

/// A provider whose every reply is computed from its request.
pub struct CallbackProvider {
    reply: Box<Reply>,
}

impl CallbackProvider {
    /// Answers each request with the events `reply` returns for it. The
    /// events should end with [`ResponseEvent::Done`].
    pub fn new(
        reply: impl Fn(&CompletionRequest) -> Vec<ResponseEvent> + Send + Sync + 'static,
    ) -> Self {
        Self {
            reply: Box::new(reply),
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl sven_model::ModelProvider for CallbackProvider {
    fn name(&self) -> &str {
        "callback-mock"
    }

    fn model_name(&self) -> &str {
        "callback-mock-model"
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let events = (self.reply)(&req);
        let wrapped: Vec<anyhow::Result<ResponseEvent>> = events.into_iter().map(Ok).collect();
        Ok(Box::pin(stream::iter(wrapped)))
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use sven_model::{Message, ModelProvider};

    /// The spec: the reply is a function of the request, so two requests
    /// that differ get different answers from the same provider.
    #[tokio::test]
    async fn the_reply_depends_on_the_request() {
        let provider = CallbackProvider::new(|req| {
            let last = req
                .messages
                .last()
                .map_or_else(String::new, |m| format!("{:?}", m.content));
            vec![
                ResponseEvent::TextDelta(format!("saw {}", last.contains("blue"))),
                ResponseEvent::Done,
            ]
        });
        for (text, expected) in [("blue sky", "saw true"), ("green grass", "saw false")] {
            let req = CompletionRequest {
                messages: vec![Message::user(text)],
                stream: true,
                ..Default::default()
            };
            let mut events = provider.complete(req).await.unwrap();
            let first = events.next().await.unwrap().unwrap();
            assert!(matches!(first, ResponseEvent::TextDelta(t) if t == expected));
        }
    }
}
