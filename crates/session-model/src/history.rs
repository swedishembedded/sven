// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Folding a [`SessionEvent`] stream back into conversation history.
//!
//! A session's authoritative record is its event stream, but the next turn has
//! to be seeded with `Message`s. This is the one place that converts between
//! them, shared by every surface that rebuilds a session per turn.

use sven_model::{reply_messages, Message};
use sven_vocab::SessionEvent;

/// Appends the messages `ev` contributes to `history`.
///
/// Every variant that contributes nothing is named explicitly rather than
/// caught by a trailing wildcard, so a new event that plausibly belongs in
/// history forces a decision here instead of being silently dropped.
pub fn reduce_history(ev: &SessionEvent, history: &mut Vec<Message>) {
    match ev {
        SessionEvent::ReplyFiled(reply) => history.extend(reply_messages(reply)),
        SessionEvent::NoteFiled(note) => history.push(Message::user(note)),
        SessionEvent::Notice(notice) => history.push(Message::user(notice.frame())),
        SessionEvent::ToolCallFinished {
            call_id, output, ..
        } => {
            history.push(Message::tool_result(call_id, output));
        }
        // Every event with no message-history representation: the reply's
        // streamed parts (the reply is filed whole, as `ReplyFiled`), a cut
        // reply (never filed), progress/usage/compaction telemetry,
        // mode/model/todo bookkeeping, questions, titles, team/subagent/peer
        // observations, harness lifecycle, and the transition trace.
        SessionEvent::TextComplete(_)
        | SessionEvent::Aborted { .. }
        | SessionEvent::TextDelta(_)
        | SessionEvent::ThinkingDelta(_)
        | SessionEvent::ThinkingComplete(_)
        | SessionEvent::ToolCallStarted(_)
        | SessionEvent::ToolProgress { .. }
        | SessionEvent::ModelWaiting { .. }
        | SessionEvent::ServedModelChanged { .. }
        | SessionEvent::Warning(_)
        | SessionEvent::ContextBudget(_)
        | SessionEvent::ContextCompacted { .. }
        | SessionEvent::TokenUsage { .. }
        | SessionEvent::ResponseSampled(_)
        | SessionEvent::TurnComplete
        | SessionEvent::Error(_)
        | SessionEvent::TodoUpdate(_)
        | SessionEvent::GoalUpdated(_)
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
        | SessionEvent::Harness { .. }
        | SessionEvent::EventsDropped { .. }
        | SessionEvent::Transition { .. } => {}
        // `SessionEvent` is non-exhaustive: a variant added later has no
        // history representation until this fold says it does.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::{ReplyFinish, SampledCall, SampledReply};

    /// A rebuilt history holds each reply as it was filed (argument text,
    /// reasoning and the server's record unchanged), each result, and each
    /// runtime note; a reply's streamed parts add nothing beside it.
    #[test]
    fn the_history_holds_each_filed_reply_as_it_arrived() {
        let reply = SampledReply {
            text: " let me look ".into(),
            reasoning: "\n hm \n".into(),
            calls: vec![SampledCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: "{ \"z\": 1,\"a\" :2.50 }".into(),
            }],
            sampled: Some(
                serde_json::value::RawValue::from_string("{\"token_ids\":\"AQ==\" }".into())
                    .unwrap(),
            ),
            finish: ReplyFinish::Stop,
        };
        let mut history = Vec::new();
        for ev in [
            SessionEvent::TextComplete(" let me look ".into()),
            SessionEvent::ReplyFiled(reply.clone()),
            SessionEvent::ToolCallFinished {
                call_id: "c1".into(),
                tool_name: "read".into(),
                output: "ok".into(),
                is_error: false,
                images: Vec::new(),
                artifacts: Vec::new(),
            },
            SessionEvent::NoteFiled("[aborted: stop]".into()),
        ] {
            reduce_history(&ev, &mut history);
        }
        let mut want = reply_messages(&reply);
        want.push(Message::tool_result("c1", "ok"));
        want.push(Message::user("[aborted: stop]"));
        assert_eq!(
            serde_json::to_string(&history).unwrap(),
            serde_json::to_string(&want).unwrap()
        );
        assert!(serde_json::to_string(&history)
            .unwrap()
            .contains("{ \\\"z\\\": 1,\\\"a\\\" :2.50 }"));
    }
}
