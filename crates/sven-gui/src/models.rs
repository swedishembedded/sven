// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Conversion helpers: sven-frontend types → Slint model types.

use slint::{ModelRc, SharedString, VecModel};
use sven_frontend::ChatSegment;
use sven_model::{MessageContent, Role};

use crate::{ChatMessage, CodeLine, SessionItem, ToastItem};

/// Build a default (all-empty) ChatMessage value.
pub fn default_chat_message(message_type: &str, content: &str, role: &str) -> ChatMessage {
    ChatMessage {
        message_type: SharedString::from(message_type),
        content: SharedString::from(content),
        role: SharedString::from(role),
        is_first_in_group: false,
        is_error: false,
        is_streaming: false,
        is_expanded: false,
        tool_name: SharedString::new(),
        tool_icon: SharedString::new(),
        tool_summary: SharedString::new(),
        tool_category: SharedString::new(),
        tool_fields_json: SharedString::new(),
        language: SharedString::new(),
        heading_level: 0,
        code_lines: ModelRc::new(VecModel::<CodeLine>::default()),
    }
}

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
                role: SharedString::from(format!("{:?}", m.role)),
                is_first_in_group: message_type != "tool-call" && message_type != "tool-result",
                is_error,
                tool_name: SharedString::from(tool_name),
                ..default_chat_message("", "", "")
            })
        }
        ChatSegment::Thinking { content } => Some(ChatMessage {
            message_type: SharedString::from("thinking"),
            content: SharedString::from(content.as_str()),
            role: SharedString::from("thinking"),
            is_expanded: false,
            ..default_chat_message("", "", "")
        }),
        ChatSegment::Error(msg) => Some(ChatMessage {
            message_type: SharedString::from("error"),
            content: SharedString::from(msg.as_str()),
            role: SharedString::from("error"),
            is_error: true,
            ..default_chat_message("", "", "")
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
            role: SharedString::from("system"),
            ..default_chat_message("", "", "")
        }),
        ChatSegment::CollabEvent(ev) => Some(ChatMessage {
            message_type: SharedString::from("system"),
            content: SharedString::from(sven_core::prompts::format_collab_event(ev)),
            role: SharedString::from("system"),
            ..default_chat_message("", "", "")
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
            role: SharedString::from("system"),
            ..default_chat_message("", "", "")
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
