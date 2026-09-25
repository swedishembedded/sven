// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What an agent actually exchanged with the model, in a form an application
//! can read.
//!
//! [`AgentState`](crate::AgentState) already holds this - it is what a resumed
//! agent resumes from - but the history is a `sven-model` type, so an
//! application built on the facade alone could obtain it and had no way to
//! name it. Anything that wants to record what an agent did, derive training
//! data from a run, or show a transcript had to reach past the facade to do
//! it, which defeats the point of there being one.
//!
//! This is deliberately a small read-only view rather than a re-export of the
//! model vocabulary. An application that wants to know what was said needs
//! roles, text, tool calls and their results; it does not need image parts,
//! response-format constraints, or the rest of a provider request, and pulling
//! that surface into the facade would make every one of those types public
//! API.
//!
//! # Faithfulness over convenience
//!
//! Each [`Turn`] corresponds to exactly one message in the agent's history,
//! including the detail that a model turn making several tool calls appears as
//! several consecutive [`Turn::Assistant`] entries rather than one entry with
//! several calls. That is how the kernel stores it, and a view that quietly
//! merged them would be making a decision on behalf of a caller who may need
//! either shape - a trainer usually wants them merged, a transcript usually
//! does not. Merge in the caller, where the reason for merging is known.

use serde::{Deserialize, Serialize};
use sven_model::{Message, MessageContent, Role};

/// One tool call an assistant turn made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Correlates with the [`Turn::ToolResult`] that answers it.
    pub id: String,
    /// The tool the model asked for.
    pub name: String,
    /// The raw JSON text the model produced, never re-parsed and
    /// re-serialised: doing so would silently normalise key order and
    /// formatting away from what the model actually emitted, which matters to
    /// anything training on it.
    pub arguments: String,
}

/// One message in an agent's history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "turn")]
pub enum Turn {
    /// The instructions the model was given for the session.
    System {
        /// The system prompt as the model saw it.
        text: String,
    },
    /// Something the caller sent the agent.
    User {
        /// The message text.
        text: String,
    },
    /// An assistant turn: text, a tool call, or both.
    Assistant {
        /// What the model said. Empty when the turn was only a tool call.
        text: String,
        /// The calls this turn made. See the type documentation for why one
        /// model turn making several calls appears as several turns.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCallRecord>,
    },
    /// The result of one tool call, as the model was shown it.
    ToolResult {
        /// The [`ToolCallRecord::id`] this answers.
        call_id: String,
        /// The result text the model saw.
        text: String,
    },
    /// A message whose content this view cannot represent as text - today,
    /// one carrying image or other non-text parts.
    ///
    /// Reported rather than dropped. A caller deriving training data from a
    /// transcript with a silent hole in it would produce examples whose
    /// context is missing a step, and never find out.
    Opaque {
        /// The role of the message that could not be represented.
        role: String,
    },
}

pub(crate) fn from_history(history: &[Message]) -> Vec<Turn> {
    history.iter().map(turn_of).collect()
}

fn turn_of(message: &Message) -> Turn {
    match (&message.role, &message.content) {
        (Role::System, MessageContent::Text(text)) => Turn::System { text: text.clone() },
        (Role::User, MessageContent::Text(text)) => Turn::User { text: text.clone() },
        (Role::Assistant, MessageContent::Text(text)) => Turn::Assistant {
            text: text.clone(),
            tool_calls: Vec::new(),
        },
        (
            _,
            MessageContent::ToolCall {
                tool_call_id,
                function,
            },
        ) => Turn::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCallRecord {
                id: tool_call_id.clone(),
                name: function.name.clone(),
                arguments: function.arguments.clone(),
            }],
        },
        (
            _,
            MessageContent::ToolResult {
                tool_call_id,
                content,
            },
        ) => Turn::ToolResult {
            call_id: tool_call_id.clone(),
            text: content.as_text().unwrap_or_default().to_string(),
        },
        (role, _) => Turn::Opaque {
            role: format!("{role:?}").to_lowercase(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_model::{FunctionCall, ToolResultContent};

    #[test]
    fn a_plain_exchange_round_trips_into_turns() {
        let history = vec![
            Message::system("you are terse"),
            Message::user("what colour?"),
            Message::assistant("blue"),
        ];
        assert_eq!(
            from_history(&history),
            vec![
                Turn::System {
                    text: "you are terse".into()
                },
                Turn::User {
                    text: "what colour?".into()
                },
                Turn::Assistant {
                    text: "blue".into(),
                    tool_calls: Vec::new()
                },
            ]
        );
    }

    #[test]
    fn a_tool_call_keeps_its_id_name_and_raw_arguments() {
        let history = vec![
            Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: "call_0".into(),
                    function: FunctionCall {
                        name: "shell".into(),
                        arguments: r#"{"command":"./svctl status"}"#.into(),
                    },
                },
            },
            Message {
                role: Role::Tool,
                content: MessageContent::ToolResult {
                    tool_call_id: "call_0".into(),
                    content: ToolResultContent::Text("active deployment: staging".into()),
                },
            },
        ];
        let turns = from_history(&history);
        match &turns[0] {
            Turn::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls[0].id, "call_0");
                assert_eq!(tool_calls[0].name, "shell");
                // Raw text, not a re-serialised object: key order and spacing
                // are part of what the model emitted.
                assert_eq!(tool_calls[0].arguments, r#"{"command":"./svctl status"}"#);
            }
            other => panic!("expected an assistant tool call, got {other:?}"),
        }
        assert_eq!(
            turns[1],
            Turn::ToolResult {
                call_id: "call_0".into(),
                text: "active deployment: staging".into()
            }
        );
    }

    #[test]
    fn content_this_view_cannot_represent_is_reported_rather_than_dropped() {
        // A silently dropped message leaves a caller deriving training data
        // with a context that is missing a step, and no way to notice.
        let history = vec![Message {
            role: Role::User,
            content: MessageContent::ContentParts(Vec::new()),
        }];
        assert_eq!(
            from_history(&history),
            vec![Turn::Opaque {
                role: "user".into()
            }]
        );
    }

    #[test]
    fn several_calls_in_one_model_turn_stay_several_turns() {
        // The kernel stores them this way and callers need different shapes;
        // merging here would decide for them.
        let call = |id: &str| Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: id.into(),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
        };
        assert_eq!(from_history(&[call("a"), call("b")]).len(), 2);
    }
}
