// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A model that answers from a list, one item per call, in order.
//!
//! This is the whole of the "mock model" a harness case needs when it asks
//! nothing but text of it: each call to [`ModelProvider::complete`] takes the
//! next item. Running past the end is an error, never a made-up answer.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients.
//! If your team needs expertise in testing agent workflows without a live
//! model then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream;
use sven_sdk::model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};

/// One scripted answer: its text and the token usage it reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The answer text.
    pub text: String,
    /// Reported `(input, output)` tokens, when the case gives them.
    pub usage: Option<(u32, u32)>,
    /// Tool calls the model proposes after its text.
    pub tool_calls: Vec<ScriptedCall>,
}

/// A tool call a scripted answer proposes.
#[derive(Debug, Clone, PartialEq)]
pub struct ScriptedCall {
    /// The call id.
    pub id: String,
    /// The tool's name.
    pub name: String,
    /// The call's arguments.
    pub args: serde_json::Value,
}

impl Answer {
    /// An answer of plain text.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            usage: None,
            tool_calls: Vec::new(),
        }
    }

    /// An answer that proposes one tool call.
    #[must_use]
    pub fn call(id: &str, name: &str, args: serde_json::Value) -> Self {
        Self {
            text: String::new(),
            usage: None,
            tool_calls: vec![ScriptedCall {
                id: id.to_string(),
                name: name.to_string(),
                args,
            }],
        }
    }
}

/// A [`ModelProvider`] that serves [`Answer`]s in order.
pub struct SequencedModel {
    answers: Mutex<VecDeque<Answer>>,
}

impl SequencedModel {
    /// A model that will give `answers`, then fail.
    #[must_use]
    pub fn new(answers: impl IntoIterator<Item = Answer>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
        }
    }
}

#[async_trait]
impl ModelProvider for SequencedModel {
    fn name(&self) -> &str {
        "sequenced"
    }

    fn model_name(&self) -> &str {
        "sequenced-answers"
    }

    async fn complete(&self, _request: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let answer = self
            .answers
            .lock()
            .map_err(|_| anyhow::anyhow!("the answer list is poisoned"))?
            .pop_front()
            .ok_or_else(|| anyhow::anyhow!("the case's model sequence is exhausted"))?;
        let mut events = vec![Ok(ResponseEvent::TextDelta(answer.text))];
        for (index, call) in answer.tool_calls.into_iter().enumerate() {
            events.push(Ok(ResponseEvent::ToolCall {
                index: u32::try_from(index).unwrap_or(u32::MAX),
                id: call.id,
                name: call.name,
                arguments: call.args.to_string(),
            }));
        }
        if let Some((input_tokens, output_tokens)) = answer.usage {
            events.push(Ok(ResponseEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            }));
        }
        events.push(Ok(ResponseEvent::Done));
        Ok(Box::pin(stream::iter(events)))
    }
}
