// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that speak each model
// provider's wire format exactly for its clients. If your team needs
// expertise in LLM tool-calling integrations then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The conversation as turns: what a provider's wire format calls one
//! message.
//!
//! Sven keeps a model turn as several [`Message`]s: its text, then one
//! [`MessageContent::ToolCall`] per call. Every provider defines the turn as
//! ONE assistant message carrying both - OpenAI's assistant message has
//! `content` and `tool_calls`, Anthropic's has `content: [text, tool_use,
//! ...]` - and a chat template renders each message as its own turn. Sent
//! message by message, a turn that said "Let me run the tests:" and called
//! `shell` reaches the model as a turn that ended after the colon, followed
//! by a second turn holding only the call; a model reading many of those
//! learns to stop after announcing a step. Every driver therefore sends
//! [`turns`], never the raw messages.
//!
//! The tool results that answer a turn are one group as well: Anthropic and
//! Bedrock carry them in one user message, OpenAI-style formats as one
//! `tool` message each.

use crate::{FunctionCall, Message, MessageContent, Role};

/// One tool call of an assistant turn.
#[derive(Debug, Clone, Copy)]
pub struct TurnCall<'a> {
    pub id: &'a str,
    pub function: &'a FunctionCall,
}

/// One message as a provider sends it.
#[derive(Debug, Clone)]
pub enum Turn<'a> {
    /// A message that is a whole turn by itself: system, user, or an
    /// assistant reply without tool calls.
    Message(&'a Message),
    /// An assistant turn that called tools: the reasoning that preceded it
    /// and the text it opened with, if any (`Text` or `ContentParts`), then
    /// its calls in order.
    Assistant {
        reasoning: Option<&'a str>,
        content: Option<&'a MessageContent>,
        calls: Vec<TurnCall<'a>>,
    },
    /// Consecutive tool results, in order.
    ToolResults(Vec<&'a Message>),
}

/// `messages` grouped into the turns a provider sends.
///
/// An assistant text or content-parts message followed by assistant tool
/// calls is one [`Turn::Assistant`], as is a run of calls without text.
/// Text that follows calls starts a new turn: it was said after the calls,
/// not with them. A run of tool results is one [`Turn::ToolResults`].
#[must_use]
pub fn turns(messages: &[Message]) -> Vec<Turn<'_>> {
    let mut out: Vec<Turn<'_>> = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let m = &messages[i];
        let opens_turn = m.role == Role::Assistant
            && matches!(
                m.content,
                MessageContent::Text(_) | MessageContent::ContentParts(_)
            )
            && messages.get(i + 1).is_some_and(is_call);
        if opens_turn || is_call(m) {
            let reasoning = m.reasoning.as_deref();
            let content = opens_turn.then_some(&m.content);
            if opens_turn {
                i += 1;
            }
            let mut calls = Vec::new();
            while let Some(MessageContent::ToolCall {
                tool_call_id,
                function,
            }) = messages.get(i).filter(|m| is_call(m)).map(|m| &m.content)
            {
                calls.push(TurnCall {
                    id: tool_call_id,
                    function,
                });
                i += 1;
            }
            out.push(Turn::Assistant {
                reasoning,
                content,
                calls,
            });
            continue;
        }
        if matches!(m.content, MessageContent::ToolResult { .. }) {
            let mut results = Vec::new();
            while let Some(r) = messages
                .get(i)
                .filter(|r| matches!(r.content, MessageContent::ToolResult { .. }))
            {
                results.push(r);
                i += 1;
            }
            out.push(Turn::ToolResults(results));
            continue;
        }
        out.push(Turn::Message(m));
        i += 1;
    }
    out
}

fn is_call(m: &Message) -> bool {
    matches!(m.content, MessageContent::ToolCall { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: "shell".into(),
                    arguments: "{}".into(),
                },
            },
            reasoning: None,
        }
    }

    fn shape(turns: &[Turn<'_>]) -> Vec<String> {
        turns
            .iter()
            .map(|t| match t {
                Turn::Message(m) => format!("{:?}", m.role),
                Turn::Assistant { content, calls, .. } => format!(
                    "assistant(text={},calls={})",
                    content.is_some(),
                    calls.iter().map(|c| c.id).collect::<Vec<_>>().join("+")
                ),
                Turn::ToolResults(r) => format!("results({})", r.len()),
            })
            .collect()
    }

    #[test]
    fn a_turns_text_and_its_calls_are_one_turn_and_its_results_one_group() {
        let messages = vec![
            Message::system("s"),
            Message::user("fix it"),
            Message::assistant("Let me run the tests:"),
            call("a"),
            call("b"),
            Message::tool_result("a", "ok"),
            Message::tool_result("b", "ok"),
            call("c"),
            Message::tool_result("c", "ok"),
            Message::assistant("Done."),
        ];
        assert_eq!(
            shape(&turns(&messages)),
            [
                "System",
                "User",
                "assistant(text=true,calls=a+b)",
                "results(2)",
                "assistant(text=false,calls=c)",
                "results(1)",
                "Assistant",
            ]
        );
    }

    #[test]
    fn text_after_calls_is_a_turn_of_its_own() {
        let messages = vec![call("a"), Message::assistant("then this")];
        assert_eq!(
            shape(&turns(&messages)),
            ["assistant(text=false,calls=a)", "Assistant"]
        );
    }
}
