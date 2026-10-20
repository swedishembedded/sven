// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// running agents on local models you can procure our services by sending an
// email to info@swedishembedded.com.
//! One more try for a request the server says does not fit its context.

use sven_hsm::{ObservationSink, UiEvent};
use sven_model::{Message, ModelProvider, ResponseFormat, Role, ToolSchema};
use sven_turn::{stream_turn, StreamedTurn, TurnLimits};
use tokio::sync::mpsc;

/// One model request, as [`stream_turn`] takes it.
pub(super) struct Request {
    pub(super) messages: Vec<Message>,
    pub(super) tools: Vec<ToolSchema>,
    pub(super) cache_key: Option<String>,
    pub(super) dynamic_suffix: Option<String>,
    pub(super) response_format: Option<ResponseFormat>,
    pub(super) max_output_tokens_override: Option<u32>,
}

impl Request {
    async fn stream(
        self,
        model: &dyn ModelProvider,
        limits: TurnLimits,
        tx: &mpsc::Sender<UiEvent>,
    ) -> anyhow::Result<StreamedTurn> {
        stream_turn(
            model,
            self.messages,
            self.tools,
            self.cache_key,
            self.dynamic_suffix,
            self.response_format,
            self.max_output_tokens_override,
            limits,
            tx,
        )
        .await
    }

    /// [`stream_turn`], and when the server rejects the request as too long
    /// for its context (the estimate undercounted, or the window shrank under
    /// us): announce it, apply `emergency_compact` to that request and ask
    /// once more. A second rejection is returned as the failure it is.
    pub(super) async fn stream_retrying_overflow(
        self,
        model: &dyn ModelProvider,
        limits: TurnLimits,
        keep_recent: usize,
        tx: &mpsc::Sender<UiEvent>,
        obs: &ObservationSink,
    ) -> anyhow::Result<StreamedTurn> {
        let again = Request {
            messages: self.messages.clone(),
            tools: self.tools.clone(),
            cache_key: self.cache_key.clone(),
            dynamic_suffix: self.dynamic_suffix.clone(),
            response_format: self.response_format.clone(),
            max_output_tokens_override: self.max_output_tokens_override,
        };
        match self.stream(model, limits, tx).await {
            Err(e) if sven_model::budget::is_context_overflow(&format!("{e:#}")) => {
                obs.emit(UiEvent::Warning(
                    "the server rejected the request as too long for its context; \
                     dropping older history and retrying once"
                        .into(),
                ));
                let mut shrunk = again.messages;
                let system = shrunk.first().filter(|m| m.role == Role::System).cloned();
                sven_turn::emergency_compact(&mut shrunk, system, keep_recent);
                Request {
                    messages: shrunk,
                    ..again
                }
                .stream(model, limits, tx)
                .await
            }
            other => other,
        }
    }
}
