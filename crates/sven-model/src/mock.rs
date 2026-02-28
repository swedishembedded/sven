// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream;

use crate::{catalog::InputModality, provider::ResponseStream, CompletionRequest, ResponseEvent};

/// Deterministic mock provider for tests.  Echoes the last user message
/// back as the assistant response.
#[derive(Default)]
pub struct MockProvider;

#[async_trait]
impl crate::ModelProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }
    fn model_name(&self) -> &str {
        "mock-model"
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let reply = req
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::Role::User))
            .and_then(|m| m.as_text())
            .unwrap_or("[no input]")
            .to_string();

        let events: Vec<anyhow::Result<ResponseEvent>> = vec![
            Ok(ResponseEvent::TextDelta(format!("MOCK: {reply}"))),
            Ok(ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            }),
            Ok(ResponseEvent::Done),
        ];
        Ok(Box::pin(stream::iter(events)))
    }
}

/// A pre-scripted mock provider.  Each call to `complete` pops the next
/// response script from the front of the queue.  This lets tests specify
/// exact event sequences – including tool calls – without network access.
pub struct ScriptedMockProvider {
    scripts: Arc<Mutex<Vec<Vec<ResponseEvent>>>>,
    name: String,
    /// Claimed input modalities.  Defaults to `[Text]` (conservative).
    modalities: Vec<InputModality>,
    /// The last `CompletionRequest` seen by this provider.
    /// Written on each `complete()` call so tests can inspect what was sent.
    pub last_request: Arc<Mutex<Option<CompletionRequest>>>,
}

impl ScriptedMockProvider {
    /// Build a provider from a list of response scripts.
    /// The outer `Vec` is the ordered list of calls; the inner `Vec` is the
    /// sequence of [`ResponseEvent`]s emitted for that call.
    pub fn new(scripts: Vec<Vec<ResponseEvent>>) -> Self {
        Self {
            scripts: Arc::new(Mutex::new(scripts)),
            name: "scripted-mock".into(),
            modalities: vec![InputModality::Text],
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// Declare that this mock supports image input as well as text.
    ///
    /// Use this in tests that exercise multimodal code paths so that
    /// `strip_images_if_unsupported` does **not** strip images before they
    /// reach the provider.
    pub fn with_vision(mut self) -> Self {
        self.modalities = vec![InputModality::Text, InputModality::Image];
        self
    }

    /// Convenience: provider that always returns a single text reply.
    pub fn always_text(reply: impl Into<String>) -> Self {
        let r = reply.into();
        Self::new(vec![vec![
            ResponseEvent::TextDelta(r),
            ResponseEvent::Usage {
                input_tokens: 5,
                output_tokens: 5,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            ResponseEvent::Done,
        ]])
    }

    /// Convenience: provider that returns a tool call followed by a text reply.
    pub fn tool_then_text(
        tool_id: impl Into<String>,
        tool_name: impl Into<String>,
        args_json: impl Into<String>,
        final_text: impl Into<String>,
    ) -> Self {
        Self::new(vec![
            // Round 1 – model emits a tool call
            vec![
                ResponseEvent::ToolCall {
                    index: 0,
                    id: tool_id.into(),
                    name: tool_name.into(),
                    arguments: args_json.into(),
                },
                ResponseEvent::Done,
            ],
            // Round 2 – model responds after tool result
            vec![
                ResponseEvent::TextDelta(final_text.into()),
                ResponseEvent::Done,
            ],
        ])
    }
}

#[async_trait]
impl crate::ModelProvider for ScriptedMockProvider {
    fn name(&self) -> &str {
        &self.name
    }
    fn model_name(&self) -> &str {
        "scripted-mock-model"
    }

    fn input_modalities(&self) -> Vec<InputModality> {
        self.modalities.clone()
    }

    async fn complete(&self, req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        *self.last_request.lock().unwrap() = Some(req);
        let events = {
            let mut scripts = self.scripts.lock().unwrap();
            if scripts.is_empty() {
                // Default fallback when all scripts are consumed
                vec![
                    ResponseEvent::TextDelta("[no more scripts]".into()),
                    ResponseEvent::Done,
                ]
            } else {
                scripts.remove(0)
            }
        };
        let wrapped: Vec<anyhow::Result<ResponseEvent>> = events.into_iter().map(Ok).collect();
        Ok(Box::pin(stream::iter(wrapped)))
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use futures::StreamExt;

    use super::*;
    use crate::{CompletionRequest, Message, ModelProvider, ResponseEvent};

    fn empty_req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Message::user("hi")],
            stream: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn mock_echoes_last_user_message() {
        let p = MockProvider;
        let mut stream = p.complete(empty_req()).await.unwrap();
        let first = stream.next().await.unwrap().unwrap();
        match first {
            ResponseEvent::TextDelta(t) => assert!(t.contains("MOCK: hi")),
            other => panic!("unexpected first event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mock_ends_with_done() {
        let p = MockProvider;
        let mut stream = p.complete(empty_req()).await.unwrap();
        let mut events = Vec::new();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        assert!(matches!(events.last(), Some(ResponseEvent::Done)));
    }

    #[tokio::test]
    async fn scripted_single_text_reply() {
        let p = ScriptedMockProvider::always_text("hello world");
        let mut stream = p.complete(empty_req()).await.unwrap();
        let ev = stream.next().await.unwrap().unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t == "hello world"));
    }

    #[tokio::test]
    async fn scripted_tool_then_text_two_rounds() {
        let p =
            ScriptedMockProvider::tool_then_text("call-1", "shell", r#"{"command":"ls"}"#, "done");

        // Round 1
        let req = empty_req();
        let mut events = Vec::new();
        let mut stream = p.complete(req.clone()).await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev.unwrap());
        }
        assert!(events
            .iter()
            .any(|e| matches!(e, ResponseEvent::ToolCall { name, .. } if name == "shell")));

        // Round 2
        let mut events2 = Vec::new();
        let mut stream2 = p.complete(req).await.unwrap();
        while let Some(ev) = stream2.next().await {
            events2.push(ev.unwrap());
        }
        assert!(events2
            .iter()
            .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t == "done")));
    }

    #[tokio::test]
    async fn scripted_fallback_when_scripts_exhausted() {
        let p = ScriptedMockProvider::new(vec![]);
        let mut stream = p.complete(empty_req()).await.unwrap();
        let ev = stream.next().await.unwrap().unwrap();
        assert!(matches!(ev, ResponseEvent::TextDelta(t) if t.contains("no more scripts")));
    }
}
