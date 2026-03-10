// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OpenAI-compatible request building: message serialisation and role mapping.

use serde_json::{json, Value};

use crate::Role;

pub(crate) fn role_str(r: &Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// Convert a slice of [`Message`]s into the OpenAI wire-format JSON array.
///
/// Extracted as a free function so it can be unit-tested without making HTTP
/// requests.
///
/// **Parallel tool call coalescing**: OpenAI requires that all tool calls from
/// one assistant turn appear inside a *single* assistant message as a
/// `tool_calls` array.  Sven stores each tool call as a separate
/// `MessageContent::ToolCall` entry internally (easier to work with), so this
/// function merges consecutive `ToolCall` messages into one JSON object before
/// sending them to the API.
pub(crate) fn build_openai_messages(messages: &[crate::Message]) -> Vec<Value> {
    use crate::{ContentPart, MessageContent, ToolContentPart, ToolResultContent};

    fn tool_call_to_json(tool_call_id: &str, function: &crate::FunctionCall) -> Value {
        json!({
            "id": tool_call_id,
            "type": "function",
            "function": {
                "name": function.name,
                "arguments": function.arguments,
            }
        })
    }

    fn tool_result_to_json(tool_call_id: &str, content: &ToolResultContent) -> Value {
        let wire_content: Value = match content {
            ToolResultContent::Text(t) => json!(t),
            ToolResultContent::Parts(parts) if !parts.is_empty() => {
                let arr: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        ToolContentPart::Text { text } => json!({ "type": "text", "text": text }),
                        ToolContentPart::Image { image_url } => json!({
                            "type": "image_url",
                            "image_url": { "url": image_url },
                        }),
                    })
                    .collect();
                json!(arr)
            }
            ToolResultContent::Parts(_) => json!(""),
        };
        json!({ "role": "tool", "tool_call_id": tool_call_id, "content": wire_content })
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len());
    let mut i = 0;

    while i < messages.len() {
        let m = &messages[i];

        // Merge consecutive ToolCall messages into one assistant message so
        // the wire format satisfies OpenAI's parallel-tool-call contract.
        if let MessageContent::ToolCall {
            tool_call_id,
            function,
        } = &m.content
        {
            let mut calls = vec![tool_call_to_json(tool_call_id, function)];
            i += 1;
            while i < messages.len() {
                if let MessageContent::ToolCall {
                    tool_call_id,
                    function,
                } = &messages[i].content
                {
                    calls.push(tool_call_to_json(tool_call_id, function));
                    i += 1;
                } else {
                    break;
                }
            }
            result.push(json!({ "role": "assistant", "tool_calls": calls }));
            continue;
        }

        let v = match &m.content {
            MessageContent::Text(t) => json!({
                "role": role_str(&m.role),
                "content": t,
            }),
            MessageContent::ContentParts(parts) if !parts.is_empty() => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
                        ContentPart::Image { image_url, detail } => {
                            let mut img_obj = json!({ "url": image_url });
                            if let Some(d) = detail {
                                img_obj["detail"] = json!(d);
                            }
                            json!({ "type": "image_url", "image_url": img_obj })
                        }
                    })
                    .collect();
                json!({ "role": role_str(&m.role), "content": content })
            }
            MessageContent::ContentParts(_) => {
                json!({ "role": role_str(&m.role), "content": "" })
            }
            MessageContent::ToolCall { .. } => unreachable!("handled above"),
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } => tool_result_to_json(tool_call_id, content),
        };
        result.push(v);
        i += 1;
    }

    result
}
