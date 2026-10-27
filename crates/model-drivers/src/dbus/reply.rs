// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Decoding a brain `Run` reply into sven response events.

use serde_json::Value;
use tracing::warn;

use sven_model::ResponseEvent;

use crate::openai_compat::served_identity;

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

/// The token-level facts and the serving identity a `Run` reply reports, as
/// events: the prompt's token ids, the account of the decode (seed, adapter,
/// base digest, backend, engine) and the sampled ids with their
/// log-probabilities. brain returns the token facts only when the request
/// asked (`logprobs`, `return_token_ids`), under the same keys its HTTP
/// surface reads: `prompt_token_ids`, `completion_token_ids` and
/// `completion_logprobs`.
pub(super) fn reply_extras(result_json: &str) -> Vec<ResponseEvent> {
    let Ok(result) = serde_json::from_str::<Value>(result_json) else {
        return Vec::new();
    };
    let ids = |key: &str| -> Vec<i64> {
        result
            .get(key)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_i64).collect())
    };
    let mut events = Vec::new();
    let prompt = ids("prompt_token_ids");
    if !prompt.is_empty() {
        events.push(ResponseEvent::PromptTokens { ids: prompt });
    }
    if let Some(identity) = served_identity(&result) {
        events.push(ResponseEvent::Identity(identity));
    }
    let completion = ids("completion_token_ids");
    let logprobs: Vec<f64> = result
        .get("completion_logprobs")
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |a| a.iter().filter_map(Value::as_f64).collect());
    if !completion.is_empty() || !logprobs.is_empty() {
        events.push(ResponseEvent::Tokens {
            ids: completion,
            logprobs,
        });
    }
    events
}

/// Token counts of a reply, in sven's terms: `input` excludes what the
/// server's prefix cache served, which is `cache_read`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ReplyUsage {
    pub input: u32,
    pub output: u32,
    pub cache_read: u32,
}

/// Best-effort token counts from the `result` JSON.
///
/// Recognises both a nested `usage` object and flat `num_tokens`-style keys;
/// anything unrecognised simply reports zero rather than failing the turn.
pub(super) fn reply_usage(result_json: &str) -> ReplyUsage {
    let Ok(v) = serde_json::from_str::<Value>(result_json) else {
        return ReplyUsage::default();
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
    let prompt = pick(
        usage,
        &["input_tokens", "prompt_tokens", "num_input_tokens"],
    );
    let cache_read = pick(usage, &["cached_tokens"]).min(prompt);
    let output = pick(
        usage,
        &[
            "output_tokens",
            "completion_tokens",
            "num_tokens",
            "num_output_tokens",
        ],
    );
    ReplyUsage {
        input: prompt - cache_read,
        output,
        cache_read,
    }
}

/// What brain's chat scanner reports it could not parse cleanly (a tool
/// call inside the reasoning, a malformed call header, a call cut off): the
/// turn may have lost a call the model made, which must never pass silently.
pub(super) fn scan_anomalies(result_json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(result_json)
        .ok()
        .and_then(|v| v.get("scan_anomalies").cloned())
        .and_then(|a| serde_json::from_value(a).ok())
        .unwrap_or_default()
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
    fn token_facts_and_identity_come_from_the_result_outputs() {
        let result = r#"{"text":"hi","seed":7,"base_digest":"sha256:ab","backend":"cuda","engine":"brain 3.0.0",
                         "prompt_token_ids":[1,2],"completion_token_ids":[3],"completion_logprobs":[-0.5]}"#;
        let events = reply_extras(result);
        assert!(matches!(&events[0], ResponseEvent::PromptTokens { ids } if ids == &[1, 2]));
        assert!(
            matches!(&events[1], ResponseEvent::Identity(i) if i.seed == Some(7) && i.backend.as_deref() == Some("cuda"))
        );
        assert!(
            matches!(&events[2], ResponseEvent::Tokens { ids, logprobs } if ids == &[3] && logprobs == &[-0.5])
        );
    }

    #[test]
    fn a_reply_without_token_facts_has_only_its_identity() {
        let events = reply_extras(r#"{"text":"hi","seed":7}"#);
        assert!(matches!(events.as_slice(), [ResponseEvent::Identity(_)]));
        assert!(reply_extras("not json").is_empty());
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

    /// brain's `prompt_tokens` is the whole prompt and `cached_tokens` the
    /// part its prefix cache served; sven's input count excludes cache hits.
    #[test]
    fn cached_prompt_tokens_are_reported_as_cache_reads() {
        let u = reply_usage(
            r#"{"prompt_tokens": 6000, "cached_tokens": 5760, "completion_tokens": 9}"#,
        );
        assert_eq!((u.input, u.output, u.cache_read), (240, 9, 5760));
    }

    #[test]
    fn reply_usage_reads_a_nested_usage_object() {
        let u = reply_usage(r#"{"usage": {"input_tokens": 7, "output_tokens": 3}}"#);
        assert_eq!((u.input, u.output, u.cache_read), (7, 3, 0));
    }

    #[test]
    fn reply_usage_reads_flat_num_tokens() {
        let o = reply_usage(r#"{"text": "hi", "num_tokens": 5}"#).output;
        assert_eq!(o, 5);
    }

    #[test]
    fn reply_usage_is_zero_for_unparsable_json() {
        assert_eq!(reply_usage("not json"), ReplyUsage::default());
    }
}
