// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! OpenAI-compatible request building: message serialisation and role mapping.

use serde::ser::{SerializeMap, SerializeSeq};
use serde::Serialize;
use serde_json::value::RawValue;
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

/// The messages of a request in the OpenAI wire format, and the `sampled`
/// records that go with some of them.
///
/// A `sampled` record is the exact JSON a server sent with a reply; it is
/// never parsed into a [`Value`] (which would rewrite it), but written into the
/// request as it came by [`WithSampled`].
pub(crate) struct WireMessages<'a> {
    pub values: Vec<Value>,
    /// `(index into values, record)`, in order.
    pub sampled: Vec<(usize, &'a RawValue)>,
}

/// A request body whose messages carry their `sampled` records verbatim.
pub(crate) struct WithSampled<'a> {
    pub body: &'a Value,
    pub sampled: &'a [(usize, &'a RawValue)],
}

impl Serialize for WithSampled<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let Value::Object(fields) = self.body else {
            return self.body.serialize(serializer);
        };
        let mut map = serializer.serialize_map(Some(fields.len()))?;
        for (key, value) in fields {
            match value.as_array().filter(|_| key == "messages") {
                Some(list) => map.serialize_entry(
                    key,
                    &Messages {
                        list,
                        sampled: self.sampled,
                    },
                )?,
                None => map.serialize_entry(key, value)?,
            }
        }
        map.end()
    }
}

struct Messages<'a> {
    list: &'a [Value],
    sampled: &'a [(usize, &'a RawValue)],
}

impl Serialize for Messages<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.list.len()))?;
        for (i, message) in self.list.iter().enumerate() {
            match (
                self.sampled.iter().find(|(at, _)| *at == i),
                message.as_object(),
            ) {
                (Some((_, raw)), Some(fields)) => {
                    seq.serialize_element(&MessageWithSampled { fields, raw })?
                }
                _ => seq.serialize_element(message)?,
            }
        }
        seq.end()
    }
}

struct MessageWithSampled<'a> {
    fields: &'a serde_json::Map<String, Value>,
    raw: &'a RawValue,
}

impl Serialize for MessageWithSampled<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len() + 1))?;
        for (key, value) in self.fields {
            map.serialize_entry(key, value)?;
        }
        map.serialize_entry("sampled", self.raw)?;
        map.end()
    }
}

/// `values` as a JSON array with each `sampled` record written verbatim
/// beside its message: the `messages` text a server that takes it as one
/// string parameter is sent.
#[cfg(feature = "dbus")]
pub(crate) fn messages_text(
    values: &[Value],
    sampled: &[(usize, &RawValue)],
) -> serde_json::Result<String> {
    serde_json::to_string(&Messages {
        list: values,
        sampled,
    })
}

/// Convert a slice of [`Message`]s into the OpenAI wire format.
///
/// **One message per model turn**: the API's assistant message carries the
/// turn's text (`content`) and all of its calls (`tool_calls`) together.
/// Sven stores the text and each call as separate messages, so this sends
/// [`sven_model::turns`] (see there for why it matters).
///
/// Each assistant turn goes back as it arrived: its text, its reasoning as
/// `reasoning_content`, each call's argument text, and the server's `sampled`
/// record, to every server alike.
pub(crate) fn build_openai_messages_with(messages: &[sven_model::Message]) -> WireMessages<'_> {
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

    /// The `tool` message of a result: its text only, which is all the
    /// OpenAI wire takes there (providers refuse an array). Its images and
    /// audio are returned for the user message that follows the turn's
    /// results.
    fn tool_result_to_json(tool_call_id: &str, content: &ToolResultContent) -> (Value, Vec<Value>) {
        let (text, media) = match content {
            ToolResultContent::Text(t) => (t.clone(), Vec::new()),
            ToolResultContent::Parts(parts) => {
                let mut text = Vec::new();
                let mut media = Vec::new();
                for part in parts {
                    match part {
                        ToolContentPart::Text { text: t } => text.push(t.as_str()),
                        ToolContentPart::Image { image_url } => media.push(json!({
                            "type": "image_url",
                            "image_url": { "url": image_url },
                        })),
                        ToolContentPart::Audio { audio_url } => {
                            media.push(audio_part_json(audio_url, None));
                        }
                    }
                }
                (text.join("\n"), media)
            }
        };
        let message = json!({ "role": "tool", "tool_call_id": tool_call_id, "content": text });
        (message, media)
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
    let mut sampled = Vec::new();
    for turn in sven_model::turns(messages) {
        match turn {
            sven_model::Turn::Message(m) => {
                let mut message = json!({
                    "role": role_str(&m.role),
                    "content": content_json(&m.content),
                });
                if let Some(r) = m.reasoning.as_deref().filter(|r| !r.is_empty()) {
                    message["reasoning_content"] = json!(r);
                }
                if let Some(raw) = m.sampled.as_deref() {
                    sampled.push((result.len(), raw));
                }
                result.push(message);
            }
            // One assistant message per model turn: the text it said and
            // every call it made (the API's `content` + `tool_calls`).
            sven_model::Turn::Assistant {
                reasoning: turn_reasoning,
                sampled: turn_sampled,
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
                if let Some(r) = turn_reasoning.filter(|r| !r.is_empty()) {
                    message["reasoning_content"] = json!(r);
                }
                if let Some(raw) = turn_sampled {
                    sampled.push((result.len(), raw));
                }
                result.push(message);
            }
            sven_model::Turn::ToolResults(results) => {
                // Media go after the whole group: a user message between two
                // tool messages would break the call/result pairing.
                let mut media = Vec::new();
                for m in results {
                    if let MessageContent::ToolResult {
                        tool_call_id,
                        content,
                    } = &m.content
                    {
                        let (message, parts) = tool_result_to_json(tool_call_id, content);
                        result.push(message);
                        if !parts.is_empty() {
                            media.push(json!({
                                "type": "text",
                                "text": format!("Returned by tool call {tool_call_id}:"),
                            }));
                            media.extend(parts);
                        }
                    }
                }
                if !media.is_empty() {
                    result.push(json!({ "role": "user", "content": media }));
                }
            }
        }
    }

    WireMessages {
        values: result,
        sampled,
    }
}

/// The message values of [`build_openai_messages_with`], as most tests want them.
#[cfg(test)]
pub(crate) fn build_openai_messages(messages: &[sven_model::Message]) -> Vec<Value> {
    build_openai_messages_with(messages).values
}

#[cfg(test)]
mod echo_tests {
    use super::*;
    use sven_model::Message;

    /// An assistant turn goes back with its reasoning and its `sampled` record
    /// to every server, the record byte for byte as it arrived.
    #[test]
    fn a_turn_goes_back_with_its_reasoning_and_its_sampled_record_verbatim() {
        let raw = r#"{"token_ids":"AQID", "finish":"stop","context_len":3}"#;
        let mut call = Message::tool_call("c1", "f", r#"{"z": 1, "a": 2.50}"#);
        call.reasoning = Some("\n think \n".into());
        call.sampled = Some(RawValue::from_string(raw.to_string()).unwrap());
        let messages = vec![Message::user("go"), call, Message::tool_result("c1", "ok")];
        let wire = build_openai_messages_with(&messages);
        let body = json!({"model": "m", "messages": wire.values});
        let text = serde_json::to_string(&WithSampled {
            body: &body,
            sampled: &wire.sampled,
        })
        .unwrap();
        let sent: Value = serde_json::from_str(&text).unwrap();
        assert!(text.contains(&format!(r#""sampled":{raw}"#)), "{text}");
        assert_eq!(sent["messages"][1]["reasoning_content"], "\n think \n");
        assert_eq!(
            sent["messages"][1]["tool_calls"][0]["function"]["arguments"],
            r#"{"z": 1, "a": 2.50}"#
        );
        assert!(sent["messages"][0].get("sampled").is_none());
    }
}
