// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! SSE stream parsing for OpenAI-compatible chat completion responses.

use serde_json::Value;

use crate::ResponseEvent;

/// Parse a single complete SSE `data:` line into a [`ResponseEvent`].
///
/// Returns `None` for empty lines, comment lines, or unparseable data.
pub(super) fn parse_sse_data_line(line: &str) -> Option<anyhow::Result<ResponseEvent>> {
    let data = line.strip_prefix("data: ")?.trim();
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return Some(Ok(ResponseEvent::Done));
    }
    let v: Value = serde_json::from_str(data).ok()?;
    Some(parse_sse_chunk(&v))
}

/// Drain all complete `\n`-terminated SSE lines from a raw-byte buffer.
///
/// Any trailing incomplete line is left in `buf`.  Because `\n` (0x0A) never
/// appears inside a multi-byte UTF-8 continuation byte, every slice ending at
/// a `\n` position is guaranteed to be complete UTF-8.  This is the canonical
/// implementation; `drain_complete_sse_lines` (the `&mut String` variant below)
/// delegates here so that tests can continue to use the simpler `String` API.
pub(crate) fn drain_complete_sse_lines_bytes(
    buf: &mut Vec<u8>,
) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events = Vec::new();
    while let Some(nl_pos) = buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = buf.drain(..=nl_pos).collect();
        // Safe: the slice ends at a '\n' byte boundary, so it is valid UTF-8.
        let line = String::from_utf8_lossy(&line_bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if let Some(ev) = parse_sse_data_line(&line) {
            events.push(ev);
        }
    }
    events
}

/// Drain all complete `\n`-terminated SSE lines from `buf`.
///
/// Any trailing incomplete line (bytes not yet terminated by `\n`) is left
/// in `buf` so it can be extended by the next TCP chunk.  This is necessary
/// because a single SSE event may be split across multiple TCP packets.
///
/// Used by tests only; production code calls `drain_complete_sse_lines_bytes`.
#[cfg(test)]
pub(crate) fn drain_complete_sse_lines(buf: &mut String) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut byte_buf = std::mem::take(buf).into_bytes();
    let events = drain_complete_sse_lines_bytes(&mut byte_buf);
    *buf = String::from_utf8(byte_buf).unwrap_or_default();
    events
}

#[cfg(test)]
pub(super) fn parse_sse_chunk_test(v: &Value) -> anyhow::Result<ResponseEvent> {
    parse_sse_chunk(v)
}

fn parse_sse_chunk(v: &Value) -> anyhow::Result<ResponseEvent> {
    // Usage-only chunk (emitted when stream_options.include_usage = true)
    if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
        // OpenAI/OpenRouter reports cached tokens in
        // prompt_tokens_details.cached_tokens.  DeepSeek V3 uses the root-level
        // prompt_cache_hit_tokens field instead.  We try the nested format
        // first and fall back to DeepSeek's flat format.
        let prompt_tokens_details = usage.get("prompt_tokens_details");
        let cache_read_tokens = prompt_tokens_details
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|t| t.as_u64())
            .or_else(|| {
                usage
                    .get("prompt_cache_hit_tokens")
                    .and_then(|t| t.as_u64())
            })
            .unwrap_or(0) as u32;
        // OpenRouter reports cache write tokens in
        // prompt_tokens_details.cache_write_tokens (non-zero when a new cache
        // entry was written, e.g. first Anthropic or Gemini turn).
        let cache_write_tokens = prompt_tokens_details
            .and_then(|d| d.get("cache_write_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0) as u32;
        // OpenAI/DeepSeek/OpenRouter report `prompt_tokens` as the grand total
        // (fresh + cache_read + cache_write).  The ResponseEvent::Usage contract
        // requires `input_tokens` to be fresh-only so that callers can compute
        // total_ctx = input + cache_read + cache_write without double-counting.
        let prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
        let fresh_input = prompt_tokens
            .saturating_sub(cache_read_tokens)
            .saturating_sub(cache_write_tokens);
        return Ok(ResponseEvent::Usage {
            input_tokens: fresh_input,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
            cache_read_tokens,
            cache_write_tokens,
        });
    }

    // llama.cpp performance metrics (top-level `timings` object)
    // These arrive in the final SSE chunk with finish_reason=stop and provide
    // cache hit counts and generation speed that are incredibly useful for CI
    // debugging.  We convert them into a Usage event so the CI runner can emit
    // them as `[sven:tokens]` trace output.
    if let Some(timings) = v.get("timings") {
        let cache_n = timings["cache_n"].as_u64().unwrap_or(0) as u32;
        let prompt_n = timings["prompt_n"].as_u64().unwrap_or(0) as u32;
        let predicted_n = timings["predicted_n"].as_u64().unwrap_or(0) as u32;

        // llama.cpp reports cache hits and fresh tokens separately.
        // `prompt_n` is the fresh-only count; `cache_n` goes into cache_read_tokens.
        return Ok(ResponseEvent::Usage {
            input_tokens: prompt_n,
            output_tokens: predicted_n,
            cache_read_tokens: cache_n,
            cache_write_tokens: 0,
        });
    }

    let choice = &v["choices"][0];

    // finish_reason=length means the model hit its output-token limit.
    // Emit MaxTokens so the agent knows any pending tool-call arguments
    // are truncated.  The [DONE] sentinel that follows will emit Done.
    if choice["finish_reason"].as_str() == Some("length") {
        return Ok(ResponseEvent::MaxTokens);
    }

    let delta = &choice["delta"];

    // Tool call delta — OpenAI may send multiple parallel tool calls in one
    // chunk, each identified by an "index" field.  We only emit the first
    // element here because each SSE chunk carries exactly one tool-call delta
    // in practice; the index routes accumulation in the agent.
    if let Some(tool_calls) = delta.get("tool_calls") {
        if let Some(tc) = tool_calls.get(0) {
            let index = tc["index"].as_u64().unwrap_or(0) as u32;
            let id = tc["id"].as_str().unwrap_or("").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let args = tc["function"]["arguments"]
                .as_str()
                .unwrap_or("")
                .to_string();
            return Ok(ResponseEvent::ToolCall {
                index,
                id,
                name,
                arguments: args,
            });
        }
    }

    // Thinking delta — two common field names for chain-of-thought reasoning:
    //   • `reasoning_content` — llama.cpp, Qwen3, DeepSeek-R1, xAI Grok-3-mini
    //   • `reasoning`         — OpenRouter (and some other aggregators)
    // Both carry the same semantics: readable CoT text that arrived before the
    // final answer.  Prefer `reasoning_content`; fall back to `reasoning`.
    let thinking_text = delta
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .or_else(|| delta.get("reasoning").and_then(|c| c.as_str()));
    if let Some(thinking) = thinking_text {
        if !thinking.is_empty() {
            return Ok(ResponseEvent::ThinkingDelta(thinking.to_string()));
        }
    }

    // Text delta
    if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
        return Ok(ResponseEvent::TextDelta(text.to_string()));
    }

    Ok(ResponseEvent::TextDelta(String::new()))
}
