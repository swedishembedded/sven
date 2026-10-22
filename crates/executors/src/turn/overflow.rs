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

/// How many tokens over its capacity a server says a request was, from a
/// rejection like "the request needs 99293 tokens but this model's serving
/// capacity is 98304 tokens". The server counts the prompt exactly and adds
/// the output it was asked for, so this is the measure of how far the
/// estimate of the prompt was off when the output budget was sized from it.
fn overshoot_tokens(error: &str) -> Option<u32> {
    needed_tokens(error)?.checked_sub(number_after(error, "capacity is ")?)
}

/// How many tokens a server says a rejected request needed (the prompt plus
/// the output it was asked for), from a message like "the request needs 99293
/// tokens but ...".
pub(super) fn needed_tokens(error: &str) -> Option<u32> {
    number_after(error, "needs ")
}

fn number_after(text: &str, key: &str) -> Option<u32> {
    let rest = &text[text.find(key)? + key.len()..];
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

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
    /// for its context (the estimate undercounted, or the window was never
    /// learned): announce it, apply `emergency_compact` to that request and
    /// ask once more. A second rejection is returned as the failure it is.
    /// The shrunk history comes back with the result, so the caller makes it
    /// the thread's: a trim that stayed with one request would be needed
    /// again by every later one, and each would lose the agent's history
    /// anew.
    pub(super) async fn stream_retrying_overflow(
        self,
        model: &dyn ModelProvider,
        limits: TurnLimits,
        todos: &[sven_vocab::TodoItem],
        tx: &mpsc::Sender<UiEvent>,
        obs: &ObservationSink,
    ) -> (anyhow::Result<StreamedTurn>, Option<Vec<Message>>) {
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
                // The prompt was larger than estimated, but the history may
                // be fine: when the response budget was sized as all the room
                // the estimate left, lower it by the overshoot and ask again
                // without dropping anything.
                let lowered = overshoot_tokens(&format!("{e:#}"))
                    .zip(again.max_output_tokens_override)
                    .and_then(|(over, out)| out.checked_sub(over + 256))
                    .filter(|&out| out >= 1024);
                if let Some(out) = lowered {
                    obs.emit(UiEvent::Warning(format!(
                        "the server counted the prompt larger than estimated; lowering the \
                         response budget to {out} tokens and retrying once"
                    )));
                    let result = Request {
                        max_output_tokens_override: Some(out),
                        ..again
                    }
                    .stream(model, limits, tx)
                    .await;
                    return (result, None);
                }
                obs.emit(UiEvent::Warning(
                    "the server rejected the request as too long for its context; \
                     dropping older history and retrying once"
                        .into(),
                ));
                let mut shrunk = again.messages;
                let system = shrunk.first().filter(|m| m.role == Role::System).cloned();
                sven_turn::emergency_compact(
                    &mut shrunk,
                    system,
                    super::compaction::EMERGENCY_KEEP,
                    todos,
                );
                let result = Request {
                    messages: shrunk.clone(),
                    ..again
                }
                .stream(model, limits, tx)
                .await;
                (result, Some(shrunk))
            }
            other => (other, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::overshoot_tokens;

    #[test]
    fn the_overshoot_is_read_from_the_servers_own_count() {
        let e = "brain error 400: the request needs 99293 tokens but this model's serving \
                 capacity is 98304 tokens; reduce the prompt or lower max_tokens";
        assert_eq!(overshoot_tokens(e), Some(989));
        assert_eq!(overshoot_tokens("context_length_exceeded"), None);
        assert_eq!(
            overshoot_tokens("needs 10 tokens, capacity is 20 tokens"),
            None
        );
    }
}
