// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes that speak each model
// provider's wire format exactly for its clients. If your team needs
// expertise in LLM tool-calling integrations then you can procure our
// services by sending an email to info@swedishembedded.com.
//! One message per turn for the block-based formats (Anthropic Messages,
//! Gemini `contents`, Bedrock Converse).
//!
//! These formats hold a turn as one message whose content is a list of
//! blocks: the model's text and its tool calls together, and every tool
//! result answering them together. Bedrock and Gemini require the roles to
//! alternate, and Anthropic documents consecutive same-role messages as one
//! turn. A driver converts each sven message to one wire message, then
//! [`coalesce`] merges each run of same-role messages into one; the OpenAI
//! style formats group with [`sven_model::turns`] instead, because their
//! assistant message has separate `content` and `tool_calls` fields.

use serde_json::{json, Value};

/// Merges consecutive messages of the same `role` into one whose `key`
/// array (`content` or `parts`) holds every block in order. A string
/// content becomes a `{"type": "text"}` / `{"text"}` block per `text_block`;
/// empty text is dropped, as these APIs refuse empty text blocks.
pub(crate) fn coalesce(
    messages: Vec<Value>,
    key: &str,
    text_block: fn(&str) -> Value,
) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        let same_role = out
            .last()
            .is_some_and(|last: &Value| last.get("role") == message.get("role"));
        if !same_role {
            out.push(message);
            continue;
        }
        let Some(last) = out.last_mut() else { continue };
        let mut blocks = blocks_of(last, key, text_block);
        blocks.extend(blocks_of(&message, key, text_block));
        last[key] = Value::Array(blocks);
    }
    out
}

fn blocks_of(message: &Value, key: &str, text_block: fn(&str) -> Value) -> Vec<Value> {
    match message.get(key) {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(s)) if !s.is_empty() => vec![text_block(s)],
        _ => Vec::new(),
    }
}

/// An Anthropic or Bedrock-style text block.
pub(crate) fn typed_text(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_of_same_role_messages_is_one_message_with_every_block_in_order() {
        let messages = vec![
            json!({"role": "user", "content": "fix it"}),
            json!({"role": "assistant", "content": "Let me look:"}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "a"}]}),
            json!({"role": "assistant", "content": [{"type": "tool_use", "id": "b"}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a"}]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "b"}]}),
            json!({"role": "user", "content": ""}),
        ];
        let out = coalesce(messages, "content", typed_text);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(out[0]["content"], "fix it", "a lone message is unchanged");
        let turn = out[1]["content"].as_array().unwrap();
        assert_eq!(turn[0], typed_text("Let me look:"));
        assert_eq!(
            (turn[1]["id"].clone(), turn[2]["id"].clone()),
            (json!("a"), json!("b"))
        );
        assert_eq!(
            out[2]["content"].as_array().unwrap().len(),
            2,
            "empty text dropped"
        );
    }
}
