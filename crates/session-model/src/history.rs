// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Folding a [`SessionEvent`] stream back into conversation history.
//!
//! A session's authoritative record is its event stream, but the next turn has
//! to be seeded with `Message`s. This is the one place that converts between
//! them, shared by every surface that rebuilds a session per turn.

use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_vocab::SessionEvent;

/// Appends the messages `ev` contributes to `history`.
///
/// Every variant that contributes nothing is named explicitly rather than
/// caught by a trailing wildcard, so a new event that plausibly belongs in
/// history forces a decision here instead of being silently dropped.
pub fn reduce_history(ev: &SessionEvent, history: &mut Vec<Message>) {
    match ev {
        SessionEvent::TextComplete(text) if !text.is_empty() => {
            history.push(Message::assistant(text));
        }
        SessionEvent::ToolCallStarted(tc) => {
            history.push(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tc.id.clone(),
                    function: FunctionCall {
                        name: tc.name.clone(),
                        arguments: tc.args.to_string(),
                    },
                },
            });
        }
        SessionEvent::ToolCallFinished {
            call_id, output, ..
        } => {
            history.push(Message::tool_result(call_id, output));
        }
        SessionEvent::Aborted { partial_text } if !partial_text.is_empty() => {
            history.push(Message::assistant(partial_text));
        }
        // Empty TextComplete/Aborted (already excluded above by the guards),
        // plus every event with no message-history representation: streaming
        // deltas (folded into the eventual TextComplete), progress/usage/
        // compaction telemetry, mode/model/todo bookkeeping, questions,
        // titles, team/subagent/peer observations, and the transition trace.
        SessionEvent::TextComplete(_)
        | SessionEvent::Aborted { .. }
        | SessionEvent::TextDelta(_)
        | SessionEvent::ThinkingDelta(_)
        | SessionEvent::ThinkingComplete(_)
        | SessionEvent::ToolProgress { .. }
        | SessionEvent::ContextCompacted { .. }
        | SessionEvent::TokenUsage { .. }
        | SessionEvent::TurnComplete
        | SessionEvent::Error(_)
        | SessionEvent::TodoUpdate(_)
        | SessionEvent::ModeChanged(_)
        | SessionEvent::ModelChanged(_)
        | SessionEvent::Question { .. }
        | SessionEvent::QuestionAnswer { .. }
        | SessionEvent::TitleGenerated(_)
        | SessionEvent::CollabEvent(_)
        | SessionEvent::DelegateSummary { .. }
        | SessionEvent::SubagentStarted { .. }
        | SessionEvent::SubagentEvent { .. }
        | SessionEvent::PeerList(_)
        | SessionEvent::Transition { .. } => {}
    }
}
