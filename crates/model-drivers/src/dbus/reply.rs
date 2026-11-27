// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Decoding a brain `Run` reply into sven response events.

use serde_json::value::RawValue;
use serde_json::Value;
use tracing::warn;

use sven_model::ResponseEvent;

use crate::openai_compat::{policy_trace_events, served_identity};

/// The response events a `Run` reply carries, in the order a streamed reply
/// would emit them: the model's reasoning, its text, its tool calls, then the
/// server's `sampled` record.
///
/// brain reports reasoning as `reasoning_content` and tool calls as
/// `tool_calls`, a string of JSON text holding `{id, name, arguments}` calls
/// (`arguments` itself JSON text); the OpenAI nesting `{function: {name,
/// arguments}}` and an inline array are accepted too. Everything is taken as
/// the server wrote it: the text whatever it holds, each call (one without a
/// name or an id included: it is answered, not dropped), arguments sent as an
/// object as their text, and the `sampled` record as the exact JSON it is.
pub(super) fn reply_events(result_json: &str, text: String) -> Vec<ResponseEvent> {
    let result: Value = serde_json::from_str(result_json).unwrap_or(Value::Null);
    let mut events = Vec::new();
    if let Some(reasoning) = result.get("reasoning_content").and_then(Value::as_str) {
        if !reasoning.is_empty() {
            events.push(ResponseEvent::ThinkingDelta(reasoning.to_string()));
        }
    }
    let calls_text = match result.get("tool_calls") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Null) | None => None,
        Some(_) => raw_field(result_json, "tool_calls"),
    };
    let calls: Vec<RawCall<'_>> = calls_text
        .as_deref()
        .map(|text| {
            serde_json::from_str(text).unwrap_or_else(|e| {
                warn!(error = %e, "D-Bus reply: tool_calls is not a list of calls; ignoring it");
                Vec::new()
            })
        })
        .unwrap_or_default();
    if !text.is_empty() || calls.is_empty() {
        events.push(ResponseEvent::TextDelta(text));
    }
    for (index, call) in calls.iter().enumerate() {
        let function = call.function.as_ref();
        let name = call
            .name
            .or_else(|| function.and_then(|f| f.name))
            .unwrap_or_default();
        let arguments = call
            .arguments
            .or_else(|| function.and_then(|f| f.arguments));
        events.push(ResponseEvent::ToolCall {
            index: index as u32,
            id: call
                .id
                .map_or_else(|| format!("call_{index}"), str::to_owned),
            name: name.to_string(),
            arguments: arguments.map(argument_text).unwrap_or_default(),
        });
    }
    if let Some(raw) = raw_field(result_json, "sampled").filter(|raw| raw != "null") {
        if let Ok(raw) = RawValue::from_string(raw) {
            events.push(ResponseEvent::Sampled(raw));
        }
    }
    events
}

/// One call as the reply wrote it, its arguments untouched.
#[derive(serde::Deserialize)]
struct RawCall<'a> {
    #[serde(borrow, default)]
    id: Option<&'a str>,
    #[serde(borrow, default)]
    name: Option<&'a str>,
    #[serde(borrow, default)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow, default)]
    function: Option<RawFunction<'a>>,
}

#[derive(serde::Deserialize)]
struct RawFunction<'a> {
    #[serde(borrow, default)]
    name: Option<&'a str>,
    #[serde(borrow, default)]
    arguments: Option<&'a RawValue>,
}

/// A call's argument text: a JSON string's text, or the text of whatever else
/// the server wrote there.
fn argument_text(raw: &RawValue) -> String {
    serde_json::from_str::<String>(raw.get()).unwrap_or_else(|_| raw.get().to_string())
}

/// The text of the top-level field `key` of `json`, as written.
fn raw_field(json: &str, key: &str) -> Option<String> {
    let map: std::collections::HashMap<&str, &RawValue> = serde_json::from_str(json).ok()?;
    map.get(key).map(|raw| raw.get().to_string())
}

/// The serving identity and the capture a `Run` reply reports, as events: the
/// account of the decode (seed, adapter, base digest, backend, engine) and,
/// when the request asked for it, brain's `policy_trace` object, read exactly
/// as the HTTP surface's is: the prompt and generated ids, the policy's own
/// log-probabilities and the temperature they were scored at.
pub(super) fn reply_extras(result_json: &str) -> anyhow::Result<Vec<ResponseEvent>> {
    let Ok(result) = serde_json::from_str::<Value>(result_json) else {
        return Ok(Vec::new());
    };
    let mut events = Vec::new();
    let mut identity = served_identity(&result);
    if let Some(trace) = result.get("policy_trace").filter(|t| !t.is_null()) {
        policy_trace_events(trace, &mut events, identity.get_or_insert_default())?;
    }
    if let Some(identity) = identity {
        events.push(ResponseEvent::Identity(identity));
    }
    Ok(events)
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
    fn the_policy_trace_and_identity_come_from_the_result_outputs() {
        let result = r#"{"text":"hi","seed":7,"base_digest":"sha256:ab","backend":"cuda","engine":"brain 3.0.0",
                         "policy_trace":{"prompt_token_ids":[1,2],"generation_token_ids":[3,4],
                                         "generation_log_probs":[-0.5,-0.25],"log_prob_temperature":1.0}}"#;
        let events = reply_extras(result).unwrap();
        assert!(matches!(&events[0], ResponseEvent::PromptTokens { ids } if ids == &[1, 2]));
        assert!(
            matches!(&events[1], ResponseEvent::Tokens { ids, logprobs } if ids == &[3, 4] && logprobs == &[-0.5, -0.25])
        );
        assert!(matches!(
            &events[2],
            ResponseEvent::Identity(i) if i.seed == Some(7)
                && i.backend.as_deref() == Some("cuda")
                && i.logprob_kind == Some(sven_vocab::LogprobKind::Policy)
                && i.logprob_temperature == Some(1.0)
        ));
    }

    #[test]
    fn the_samplers_top_list_comes_from_the_policy_trace_and_a_malformed_one_fails() {
        let trace = |top: &str| {
            format!(
                r#"{{"policy_trace":{{"prompt_token_ids":[1],"generation_token_ids":[3,4],
                    "generation_log_probs":[-0.5,-0.25],"log_prob_temperature":1.0,{top}}}}}"#
            )
        };
        let events = reply_extras(&trace(
            r#""top_log_probs":2,"generation_top_ids":[[3,9],[4,8]],"generation_top_log_probs":[[-0.5,-1.0],[-0.25,-2.0]]"#,
        ))
        .unwrap();
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TopTokens { ids, logprobs }
            if ids == &[vec![3, 9], vec![4, 8]] && logprobs == &[vec![-0.5, -1.0], vec![-0.25, -2.0]])));
        // A trace that was not asked for a top list has none, and that is not an error here.
        let plain = reply_extras(&trace(r#""x":0"#)).unwrap();
        assert!(!plain
            .iter()
            .any(|e| matches!(e, ResponseEvent::TopTokens { .. })));
        // A list that does not cover every generated token, or only half of the pair, fails the reply.
        for bad in [
            r#""generation_top_ids":[[3]],"generation_top_log_probs":[[-0.5]]"#,
            r#""generation_top_ids":[[3],[4]]"#,
            r#""generation_top_ids":[[3,1],[4]],"generation_top_log_probs":[[-0.5],[-0.25]]"#,
        ] {
            assert!(reply_extras(&trace(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_samplers_support_comes_from_the_policy_trace_and_a_malformed_one_fails() {
        let trace = |support: &str| {
            format!(
                r#"{{"policy_trace":{{"prompt_token_ids":[1],"generation_token_ids":[3,4],
                    "generation_log_probs":[-0.5,-0.25],"log_prob_temperature":1.0,{support}}}}}"#
            )
        };
        let good = r#""support_cap":3,"generation_sampler_log_probs":[-0.1,-0.2],"generation_support_offsets":[0,1,3],"generation_support_ids":[3,4,5]"#;
        let events = reply_extras(&trace(good)).unwrap();
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::SamplerSupport(s)
            if s.cap == 3 && s.log_probs == [-0.1, -0.2] && s.offsets == [0, 1, 3] && s.ids == [3, 4, 5])));
        let short = r#""support_cap":3,"generation_sampler_log_probs":[-0.1,-0.2],"generation_support_offsets":[0,1],"generation_support_ids":[3]"#;
        assert!(reply_extras(&trace(short)).is_err());
    }

    #[test]
    fn a_reply_without_token_facts_has_only_its_identity() {
        let events = reply_extras(r#"{"text":"hi","seed":7}"#).unwrap();
        assert!(matches!(events.as_slice(), [ResponseEvent::Identity(_)]));
        assert!(reply_extras("not json").unwrap().is_empty());
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
        assert!(
            matches!(&events[1], ResponseEvent::TextDelta(t) if t == "\n\n"),
            "the text is the reply's, whitespace too"
        );
        assert_eq!(
            tool_call(&events[2]),
            (0, "call_ae82_0", "read_file", r#"{"path": "a.txt"}"#)
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

    /// A call without a name is the model's and stays in the reply; it runs
    /// nothing, and the model is told.
    #[test]
    fn a_call_without_a_name_is_kept() {
        let result = r#"{"tool_calls": [{"id": "c1", "function": {"arguments": "{}"}}]}"#;
        let events = reply_events(result, String::new());
        assert_eq!(tool_call(&events[0]), (0, "c1", "", "{}"));
    }

    /// Arguments sent as an object keep their text, key order and spacing,
    /// and the `sampled` record arrives as the exact JSON the server wrote.
    #[test]
    fn a_reply_is_read_as_the_server_wrote_it() {
        let result = r#"{"text":"","tool_calls":[{"id":"c1","name":"f","arguments":{"z": 1, "a" :2.50}}],"sampled":{"token_ids":"AQI=", "finish":"stop"}}"#;
        let events = reply_events(result, String::new());
        assert_eq!(
            tool_call(&events[0]),
            (0, "c1", "f", r#"{"z": 1, "a" :2.50}"#)
        );
        assert!(
            matches!(&events[1], ResponseEvent::Sampled(raw) if raw.get() == r#"{"token_ids":"AQI=", "finish":"stop"}"#)
        );
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
