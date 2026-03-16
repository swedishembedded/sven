// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Conversion helpers: sven-frontend types → Slint model types.
//!
//! Slint requires data to be provided as `VecModel<T>` where `T` is a struct
//! generated from the `.slint` definitions. These functions convert from the
//! domain types in `sven-frontend` to the generated Slint structs.

use slint::{ModelRc, SharedString, VecModel};
use sven_frontend::ChatSegment;
use sven_model::{MessageContent, Role};

use crate::{ChatMessage, SessionItem, ToastItem};

/// Convert a `ChatSegment` to a `ChatMessage` for the Slint model.
pub fn segment_to_chat_message(seg: &ChatSegment) -> Option<ChatMessage> {
    match seg {
        ChatSegment::Message(m) => {
            let (message_type, content, tool_name, is_error): (&str, String, String, bool) =
                match (&m.role, &m.content) {
                    (Role::User, MessageContent::Text(t)) => {
                        ("user", t.clone(), String::new(), false)
                    }
                    (Role::Assistant, MessageContent::Text(t)) => {
                        ("assistant", t.clone(), String::new(), false)
                    }
                    (Role::Assistant, MessageContent::ToolCall { function, .. }) => {
                        let args_preview = function.arguments.chars().take(200).collect::<String>();
                        ("tool-call", args_preview, function.name.clone(), false)
                    }
                    (Role::Tool, MessageContent::ToolResult { content, .. }) => {
                        let text = content.to_string();
                        let preview = text.chars().take(500).collect::<String>();
                        ("tool-result", preview, String::new(), false)
                    }
                    _ => return None,
                };
            Some(ChatMessage {
                message_type: SharedString::from(message_type),
                content: SharedString::from(content),
                tool_name: SharedString::from(tool_name),
                is_error,
                is_streaming: false,
                role: SharedString::from(format!("{:?}", m.role)),
            })
        }
        ChatSegment::Thinking { content } => Some(ChatMessage {
            message_type: SharedString::from("thinking"),
            content: SharedString::from(content.as_str()),
            tool_name: SharedString::new(),
            is_error: false,
            is_streaming: false,
            role: SharedString::from("thinking"),
        }),
        ChatSegment::Error(msg) => Some(ChatMessage {
            message_type: SharedString::from("error"),
            content: SharedString::from(msg.as_str()),
            tool_name: SharedString::new(),
            is_error: true,
            is_streaming: false,
            role: SharedString::from("error"),
        }),
        ChatSegment::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            ..
        } => Some(ChatMessage {
            message_type: SharedString::from("system"),
            content: SharedString::from(format!(
                "Context compacted ({strategy}): {tokens_before} → {tokens_after} tokens"
            )),
            tool_name: SharedString::new(),
            is_error: false,
            is_streaming: false,
            role: SharedString::from("system"),
        }),
        ChatSegment::CollabEvent(ev) => Some(ChatMessage {
            message_type: SharedString::from("system"),
            content: SharedString::from(sven_core::prompts::format_collab_event(ev)),
            tool_name: SharedString::new(),
            is_error: false,
            is_streaming: false,
            role: SharedString::from("system"),
        }),
        ChatSegment::DelegateSummary {
            to_name,
            task_title,
            status,
            result_preview,
            ..
        } => Some(ChatMessage {
            message_type: SharedString::from("system"),
            content: SharedString::from(format!(
                "Delegated \"{task_title}\" to {to_name}: {status} — {result_preview}"
            )),
            tool_name: SharedString::new(),
            is_error: false,
            is_streaming: false,
            role: SharedString::from("system"),
        }),
        ChatSegment::TodoUpdate(_) => None,
    }
}

/// Convert a list of `ChatSegment`s to a Slint `VecModel<ChatMessage>`.
pub fn segments_to_model(segments: &[ChatSegment]) -> ModelRc<ChatMessage> {
    let items: Vec<ChatMessage> = segments
        .iter()
        .filter_map(segment_to_chat_message)
        .collect();
    ModelRc::new(VecModel::from(items))
}

/// Build a `ToastItem` from a notification string and level.
pub fn make_toast(message: impl Into<String>, level: &str) -> ToastItem {
    ToastItem {
        message: SharedString::from(message.into()),
        level: SharedString::from(level),
    }
}

/// Build an empty Slint messages model.
pub fn empty_messages_model() -> ModelRc<ChatMessage> {
    ModelRc::new(VecModel::from(vec![]))
}

/// Build an empty Slint sessions model.
pub fn empty_sessions_model() -> ModelRc<SessionItem> {
    ModelRc::new(VecModel::from(vec![]))
}
