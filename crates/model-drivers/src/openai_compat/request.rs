// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OpenAI-compatible request building: message serialisation and role mapping.

use serde_json::{json, Value};

use sven_model::Role;

/// Build an OpenAI `input_audio` content part.
///
/// The OpenAI wire format carries the *bare* base64 payload (no `data:` URL
/// prefix) plus a `format` discriminator:
///
/// ```json
/// { "type": "input_audio", "input_audio": { "data": "<b64>", "format": "wav" } }
/// ```
///
/// When `audio_url` is not a data URL (a plain HTTPS reference) there is
/// nothing standard to send, so the part degrades to a text note rather than
/// inventing a non-standard field.
fn audio_part_json(audio_url: &str, format: Option<&str>) -> Value {
    let derived = sven_model::ContentPart::Audio {
        audio_url: audio_url.to_string(),
        format: format.map(|f| f.to_string()),
    };
    let fmt = derived.audio_format().to_string();
    match sven_model::parse_data_url_parts(audio_url) {
        Ok((_mime, b64)) => json!({
            "type": "input_audio",
            "input_audio": { "data": b64, "format": fmt },
        }),
        Err(_) => json!({
            "type": "text",
            "text": format!("[audio omitted: not an inline data URL: {audio_url}]"),
        }),
    }
}

/// One tool definition in the OpenAI function-calling wire format, the shape
/// every OpenAI-compatible server (and brain's chat actions) accepts.
pub(crate) fn function_tool(t: &sven_model::ToolSchema) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.parameters,
        }
    })
}

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
/// **One message per model turn**: the API's assistant message carries the
/// turn's text (`content`) and all of its calls (`tool_calls`) together.
/// Sven stores the text and each call as separate messages, so this sends
/// [`sven_model::turns`] (see there for why it matters).
///
/// With `reasoning` set, each assistant turn's reasoning goes along as
/// `reasoning_content` (the field local servers' chat templates read).
pub(crate) fn build_openai_messages_with(
    messages: &[sven_model::Message],
    reasoning: bool,
) -> Vec<Value> {
    use sven_model::{ContentPart, MessageContent, ToolContentPart, ToolResultContent};

    fn tool_call_to_json(tool_call_id: &str, function: &sven_model::FunctionCall) -> Value {
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
                        ToolContentPart::Audio { audio_url } => audio_part_json(audio_url, None),
                    })
                    .collect();
                json!(arr)
            }
            ToolResultContent::Parts(_) => json!(""),
        };
        json!({ "role": "tool", "tool_call_id": tool_call_id, "content": wire_content })
    }

    fn content_json(content: &MessageContent) -> Value {
        match content {
            MessageContent::Text(t) => json!(t),
            MessageContent::ContentParts(parts) if !parts.is_empty() => {
                let content: Vec<Value> = parts
                    .iter()
                    .map(|p| match p {
                        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
                        ContentPart::Audio { audio_url, format } => {
                            audio_part_json(audio_url, format.as_deref())
                        }
                        ContentPart::Image { image_url, detail } => {
                            let mut img_obj = json!({ "url": image_url });
                            if let Some(d) = detail {
                                img_obj["detail"] = json!(d);
                            }
                            json!({ "type": "image_url", "image_url": img_obj })
                        }
                    })
                    .collect();
                json!(content)
            }
            MessageContent::ContentParts(_)
            | MessageContent::ToolCall { .. }
            | MessageContent::ToolResult { .. } => json!(""),
        }
    }

    let mut result: Vec<Value> = Vec::with_capacity(messages.len());
    for turn in sven_model::turns(messages) {
        match turn {
            sven_model::Turn::Message(m) => {
                let mut message = json!({
                    "role": role_str(&m.role),
                    "content": content_json(&m.content),
                });
                if let Some(r) = m
                    .reasoning
                    .as_deref()
                    .filter(|r| reasoning && !r.is_empty())
                {
                    message["reasoning_content"] = json!(r);
                }
                result.push(message);
            }
            // One assistant message per model turn: the text it said and
            // every call it made (the API's `content` + `tool_calls`).
            sven_model::Turn::Assistant {
                reasoning: turn_reasoning,
                content,
                calls,
            } => {
                let mut message = json!({
                    "role": "assistant",
                    "tool_calls": calls
                        .iter()
                        .map(|c| tool_call_to_json(c.id, c.function))
                        .collect::<Vec<_>>(),
                });
                if let Some(content) = content {
                    message["content"] = content_json(content);
                }
                if let Some(r) = turn_reasoning.filter(|r| reasoning && !r.is_empty()) {
                    message["reasoning_content"] = json!(r);
                }
                result.push(message);
            }
            sven_model::Turn::ToolResults(results) => {
                for m in results {
                    if let MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } = &m.content
                    {
                        result.push(tool_result_to_json(tool_call_id, content));
                    }
                }
            }
        }
    }

    result
}

/// [`build_openai_messages_with`] without reasoning, as most tests want it.
#[cfg(test)]
pub(crate) fn build_openai_messages(messages: &[sven_model::Message]) -> Vec<Value> {
    build_openai_messages_with(messages, false)
}
