// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Decoding a brain `Run` reply into sven response events.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::warn;

use sven_model::ResponseEvent;

use super::blob;

/// The response events a `Run` reply carries, in the order a streamed reply
/// would emit them: the model's reasoning, its text, then its tool calls.
///
/// brain reports reasoning as `reasoning_content` and tool calls as
/// `tool_calls`, a string of JSON text holding `{id, name, arguments}` calls
/// (`arguments` itself JSON text); the OpenAI nesting `{function: {name,
/// arguments}}` and an inline array are accepted too. Whitespace-only text next
/// to tool calls is no reply. A call without a name is dropped with a warning
/// rather than failing the turn; one without an id gets `call_<index>`.
pub(super) fn reply_events(result_json: &str, text: String) -> Vec<ResponseEvent> {
    let result: Value = serde_json::from_str(result_json).unwrap_or(Value::Null);
    let mut events = Vec::new();
    if let Some(reasoning) = result.get("reasoning_content").and_then(Value::as_str) {
        if !reasoning.is_empty() {
            events.push(ResponseEvent::ThinkingDelta(reasoning.to_string()));
        }
    }
    let calls = match result.get("tool_calls") {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or_else(|e| {
            warn!(error = %e, "D-Bus reply: tool_calls is not JSON; ignoring it");
            Value::Null
        }),
        Some(v) => v.clone(),
        None => Value::Null,
    };
    let calls = calls.as_array().cloned().unwrap_or_default();
    if !text.trim().is_empty() || calls.is_empty() {
        events.push(ResponseEvent::TextDelta(text));
    }
    for (index, call) in calls.iter().enumerate() {
        let field = |key: &str| {
            call.get(key)
                .or_else(|| call.pointer(&format!("/function/{key}")))
        };
        let Some(name) = field("name").and_then(Value::as_str) else {
            warn!(call = %call, "D-Bus reply: tool call without a name; dropped");
            continue;
        };
        let arguments = match field("arguments") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => v.to_string(),
            None => "{}".to_string(),
        };
        events.push(ResponseEvent::ToolCall {
            index: index as u32,
            id: call
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| format!("call_{index}"), str::to_owned),
            name: name.to_string(),
            arguments,
        });
    }
    events
}

/// Pull the generated text out of a `Run` reply.
///
/// `out_fds["text"]` is preferred; the `result` JSON's `"text"` field is the
/// fallback for servers that inline short replies.
pub(super) fn reply_text(
    result_json: &str,
    out_fds: &HashMap<String, zbus::zvariant::OwnedFd>,
) -> Result<String> {
    if let Some(fd) = out_fds.get("text") {
        return blob::read_fd_to_string(fd);
    }
    let parsed: Value = serde_json::from_str(result_json)
        .with_context(|| format!("parsing Run result JSON: {result_json:.256}"))?;
    parsed
        .get("text")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Run reply carried neither an out_fds[\"text\"] blob nor a \
                 `text` field in its result JSON: {result_json:.256}"
            )
        })
}

/// What the server reports about the generation itself, from the
/// `generation` object of `out_meta`.
pub(super) struct ReplyGeneration {
    pub(super) model: Option<String>,
    pub(super) seed: Option<u64>,
    pub(super) ids: Vec<i64>,
    pub(super) logprobs: Vec<f64>,
}

/// Parse the optional `generation` entry of `out_meta`. A server that does
/// not report one, or reports it malformed, simply yields `None`: the text
/// reply is still valid without the training-grade extras.
pub(super) fn reply_generation(out_meta: &str) -> Option<ReplyGeneration> {
    let meta: Value = serde_json::from_str(out_meta).ok()?;
    let g = meta.get("generation")?;
    let list = |key: &str| {
        g.get(key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    };
    Some(ReplyGeneration {
        model: g.get("model").and_then(|m| m.as_str()).map(str::to_owned),
        seed: g.get("seed").and_then(|s| s.as_u64()),
        ids: list("token_ids").iter().filter_map(Value::as_i64).collect(),
        logprobs: list("logprobs").iter().filter_map(Value::as_f64).collect(),
    })
}

/// Best-effort token counts from the `result` JSON.
///
/// Recognises both a nested `usage` object and flat `num_tokens`-style keys;
/// anything unrecognised simply reports zero rather than failing the turn.
pub(super) fn reply_usage(result_json: &str) -> (u32, u32) {
    let Ok(v) = serde_json::from_str::<Value>(result_json) else {
        return (0, 0);
    };
    let pick = |obj: &Value, keys: &[&str]| -> u32 {
        for k in keys {
            if let Some(n) = obj.get(*k).and_then(|x| x.as_u64()) {
                return n as u32;
            }
        }
        0
    };
    let usage = v.get("usage").unwrap_or(&v);
    let input = pick(
        usage,
        &["input_tokens", "prompt_tokens", "num_input_tokens"],
    );
    let output = pick(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "num_tokens",
            "num_output_tokens",
        ],
    );
    (input, output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(e: &ResponseEvent) -> (u32, &str, &str, &str) {
        match e {
            ResponseEvent::ToolCall {
                index,
                id,
                name,
                arguments,
            } => (*index, id, name, arguments),
            other => panic!("expected a tool call, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_reply_is_one_text_delta() {
        let events = reply_events(r#"{"text": "hi"}"#, "hi".into());
        assert!(matches!(events.as_slice(), [ResponseEvent::TextDelta(t)] if t == "hi"));
    }

    #[test]
    fn brain_tool_call_reply_becomes_reasoning_and_tool_call_events() {
        // A reply captured from brain's qwen35 `generate` action.
        let result = r#"{"finish_reason":"tool_calls","reasoning_content":"Let's use the read_file tool.","text":"\n\n","tool_calls":"[{\"arguments\":\"{\\\"path\\\": \\\"a.txt\\\"}\",\"id\":\"call_ae82_0\",\"name\":\"read_file\"}]"}"#;
        let events = reply_events(result, "\n\n".into());
        assert!(matches!(&events[0], ResponseEvent::ThinkingDelta(t) if t.contains("read_file")));
        assert_eq!(
            tool_call(&events[1]),
            (0, "call_ae82_0", "read_file", r#"{"path": "a.txt"}"#)
        );
        assert_eq!(
            events.len(),
            2,
            "whitespace beside a tool call is no text reply"
        );
    }

    #[test]
    fn tool_calls_in_a_json_string_become_tool_call_events() {
        let result = serde_json::json!({
            "text": "",
            "reasoning_content": "need the file",
            "tool_calls": r#"[{"id":"c7","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a\"}"}},
                             {"type":"function","function":{"name":"grep","arguments":{"pattern":"x"}}}]"#,
        });
        let events = reply_events(&result.to_string(), String::new());
        assert!(matches!(&events[0], ResponseEvent::ThinkingDelta(t) if t == "need the file"));
        assert_eq!(
            tool_call(&events[1]),
            (0, "c7", "read_file", r#"{"path":"a"}"#)
        );
        assert_eq!(
            tool_call(&events[2]),
            (1, "call_1", "grep", r#"{"pattern":"x"}"#)
        );
        assert_eq!(events.len(), 3, "no empty text delta beside tool calls");
    }

    #[test]
    fn a_call_without_a_name_is_dropped() {
        let result = r#"{"tool_calls": [{"id": "c1", "function": {"arguments": "{}"}}]}"#;
        let events = reply_events(result, String::new());
        assert!(events
            .iter()
            .all(|e| !matches!(e, ResponseEvent::ToolCall { .. })));
    }

    #[test]
    fn reply_text_falls_back_to_the_result_json_field() {
        let out = reply_text(r#"{"text": "hello"}"#, &HashMap::new()).unwrap();
        assert_eq!(out, "hello");
    }

    #[test]
    fn reply_text_prefers_the_out_fd() {
        let mut fds = HashMap::new();
        fds.insert(
            "text".to_string(),
            blob::memfd_with_bytes("text", b"from fd").unwrap(),
        );
        let out = reply_text(r#"{"text": "from json"}"#, &fds).unwrap();
        assert_eq!(out, "from fd");
    }

    #[test]
    fn reply_text_errors_when_there_is_no_text_anywhere() {
        let err = reply_text(r#"{"status": "ok"}"#, &HashMap::new()).unwrap_err();
        assert!(err.to_string().contains("neither"), "{err}");
    }

    #[test]
    fn reply_usage_reads_a_nested_usage_object() {
        let (i, o) = reply_usage(r#"{"usage": {"input_tokens": 7, "output_tokens": 3}}"#);
        assert_eq!((i, o), (7, 3));
    }

    #[test]
    fn reply_usage_reads_flat_num_tokens() {
        let (_, o) = reply_usage(r#"{"text": "hi", "num_tokens": 5}"#);
        assert_eq!(o, 5);
    }

    #[test]
    fn reply_usage_is_zero_for_unparsable_json() {
        assert_eq!(reply_usage("not json"), (0, 0));
    }
}
