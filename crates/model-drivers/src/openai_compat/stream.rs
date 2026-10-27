// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! SSE stream parsing for OpenAI-compatible chat completion responses.

use std::collections::VecDeque;

use anyhow::bail;
use futures::{Stream, StreamExt};
use serde_json::Value;

use sven_model::ResponseEvent;
use sven_vocab::{AdapterRef, ServedIdentity};

/// The events one SSE `data:` frame carries.
///
/// A frame is not one event: a delta may hold text *and* several tool-call
/// fragments, and the last frame may carry content, a finish reason and usage
/// together. `terminal` is true when the frame ends the generation (a finish
/// reason was given, or `[DONE]` arrived).
pub(super) struct Frame {
    pub(super) events: Vec<ResponseEvent>,
    pub(super) terminal: bool,
    /// The `model` the server reported on this frame, if any.
    pub(super) model: Option<String>,
}

/// Parse a single complete SSE line into the events it carries.
///
/// Returns `None` for blank lines, comments and non-`data` fields. A `data`
/// payload that is not JSON is an error: dropping it would silently lose
/// whatever it carried, including tool-call arguments.
pub(super) fn parse_sse_data_line(line: &str) -> Option<anyhow::Result<Frame>> {
    // `data:` may be followed by one optional space (SSE grammar).
    let data = line.strip_prefix("data:")?.trim();
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        return Some(Ok(Frame {
            events: vec![ResponseEvent::Done],
            terminal: true,
            model: None,
        }));
    }
    Some(match serde_json::from_str::<Value>(data) {
        Ok(v) => parse_sse_chunk(&v),
        Err(e) => Err(anyhow::anyhow!("malformed stream frame ({e}): {data}")),
    })
}

/// Drain all complete `\n`-terminated SSE lines from a raw-byte buffer,
/// recording in `finished` whether a terminal frame was seen.
///
/// Any trailing incomplete line is left in `buf`.  Because `\n` (0x0A) never
/// appears inside a multi-byte UTF-8 continuation byte, every slice ending at
/// a `\n` position is guaranteed to be complete UTF-8.
///
/// A chunk that completes at least one SSE line but yields no parseable event
/// (comment keepalives - `: ping` - blank lines, unknown non-`data` fields)
/// still returns one empty `TextDelta`: bytes arrived, so the connection is
/// alive and the caller's stream-idle watchdog must be reset.  The same
/// contract as the Anthropic driver, which maps the wire-level `ping` event
/// to an empty `TextDelta` for exactly this reason.  An empty `TextDelta` is
/// a no-op downstream (`stream_turn`'s non-empty guard skips it) beyond that
/// watchdog reset.
fn drain_lines(
    buf: &mut Vec<u8>,
    finished: &mut bool,
    served: &mut ServedReport,
) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut events = Vec::new();
    let mut completed_line = false;
    while let Some(nl_pos) = buf.iter().position(|&b| b == b'\n') {
        let line_bytes: Vec<u8> = buf.drain(..=nl_pos).collect();
        // Safe: the slice ends at a '\n' byte boundary, so it is valid UTF-8.
        let line = String::from_utf8_lossy(&line_bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        completed_line = true;
        match parse_sse_data_line(&line) {
            Some(Ok(frame)) => {
                *finished |= frame.terminal;
                events.extend(frame.events.into_iter().map(Ok));
                // After the frame's own events: consumers of the first
                // event of a frame (content) must not see a new lead event.
                if let Some(event) = served.observe(frame.model) {
                    events.push(Ok(event));
                }
            }
            Some(Err(e)) => events.push(Err(e)),
            None => {}
        }
    }
    if events.is_empty() && completed_line {
        events.push(Ok(ResponseEvent::TextDelta(String::new())));
    }
    events
}

/// Test-facing form of [`drain_lines`] for callers that do not track
/// completion.
#[cfg(test)]
pub(crate) fn drain_complete_sse_lines_bytes(
    buf: &mut Vec<u8>,
) -> Vec<anyhow::Result<ResponseEvent>> {
    drain_lines(buf, &mut false, &mut ServedReport::new(None))
}

/// Drain all complete `\n`-terminated SSE lines from `buf`.
///
/// Used by tests only; production code goes through [`decode_events`].
#[cfg(test)]
pub(crate) fn drain_complete_sse_lines(buf: &mut String) -> Vec<anyhow::Result<ResponseEvent>> {
    let mut byte_buf = std::mem::take(buf).into_bytes();
    let events = drain_complete_sse_lines_bytes(&mut byte_buf);
    *buf = String::from_utf8(byte_buf).unwrap_or_default();
    events
}

/// Incremental SSE decoder: raw response bytes in, [`ResponseEvent`]s out.
///
/// Keeps a raw-byte line buffer across chunks (an event may be split across
/// TCP packets, and a multi-byte UTF-8 sequence across chunk boundaries) and
/// remembers whether the generation reached a terminal frame, so the end of
/// the byte stream can be told apart from an interrupted connection.
struct SseDecoder {
    buf: Vec<u8>,
    finished: bool,
    served: ServedReport,
}

/// Emits [`ResponseEvent::Served`] once, from the first frame that names the
/// serving model. The seed is the one the request carried: servers do not
/// echo it back.
struct ServedReport {
    seed: Option<u64>,
    done: bool,
}

impl ServedReport {
    fn new(seed: Option<u64>) -> Self {
        Self { seed, done: false }
    }

    fn observe(&mut self, model: Option<String>) -> Option<ResponseEvent> {
        if self.done {
            return None;
        }
        let model = model.filter(|m| !m.is_empty())?;
        self.done = true;
        Some(ResponseEvent::Served {
            model,
            seed: self.seed,
        })
    }
}

impl SseDecoder {
    fn new(seed: Option<u64>) -> Self {
        Self {
            buf: Vec::new(),
            finished: false,
            served: ServedReport::new(seed),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<anyhow::Result<ResponseEvent>> {
        self.buf.extend_from_slice(bytes);
        drain_lines(&mut self.buf, &mut self.finished, &mut self.served)
    }

    /// The byte stream ended cleanly. Flush an unterminated last line, then
    /// fail if the generation never finished: a server that closes the
    /// connection before a finish reason or `[DONE]` has cut the answer
    /// short, and handing that on as a complete turn would execute truncated
    /// tool calls.
    fn end(&mut self) -> Vec<anyhow::Result<ResponseEvent>> {
        let mut out = Vec::new();
        if !self.buf.is_empty() {
            self.buf.push(b'\n');
            out = drain_lines(&mut self.buf, &mut self.finished, &mut self.served);
        }
        if !self.finished {
            out.push(Err(anyhow::anyhow!(
                "stream ended before the response finished (connection closed without a finish reason or [DONE])"
            )));
        }
        out
    }
}

/// Decode a response body into events, failing on a body that ends before
/// the generation did and on transport errors.
pub(crate) fn decode_events<B, E>(
    body: impl Stream<Item = Result<B, E>> + Unpin + Send + 'static,
    seed: Option<u64>,
) -> impl Stream<Item = anyhow::Result<ResponseEvent>> + Send
where
    B: AsRef<[u8]>,
    E: Into<anyhow::Error>,
{
    struct State<S> {
        body: S,
        decoder: SseDecoder,
        ready: VecDeque<anyhow::Result<ResponseEvent>>,
        closed: bool,
    }
    futures::stream::unfold(
        State {
            body,
            decoder: SseDecoder::new(seed),
            ready: VecDeque::new(),
            closed: false,
        },
        |mut st| async move {
            loop {
                if let Some(ev) = st.ready.pop_front() {
                    return Some((ev, st));
                }
                if st.closed {
                    return None;
                }
                match st.body.next().await {
                    Some(Ok(bytes)) => st.ready.extend(st.decoder.push(bytes.as_ref())),
                    Some(Err(e)) => {
                        st.ready.push_back(Err(e.into()));
                        st.closed = true;
                    }
                    None => {
                        st.ready.extend(st.decoder.end());
                        st.closed = true;
                    }
                }
            }
        },
    )
}

#[cfg(test)]
pub(super) fn parse_sse_chunk_test(v: &Value) -> anyhow::Result<ResponseEvent> {
    Ok(parse_sse_chunk(v)?
        .events
        .into_iter()
        .next()
        .expect("a frame always carries at least one event"))
}

/// The server's account of a decode, from a frame's non-standard `brain`
/// object: `seed`, `adapter {id, digest}`, `base_digest`, `backend`, `engine`.
/// `None` when it names none of them.
pub(crate) fn served_identity(brain: &Value) -> Option<ServedIdentity> {
    let text = |key: &str| brain.get(key).and_then(Value::as_str).map(str::to_owned);
    let identity = ServedIdentity {
        seed: brain.get("seed").and_then(Value::as_u64),
        adapter: brain.get("adapter").and_then(|a| {
            Some(AdapterRef {
                id: a.get("id")?.as_str()?.to_owned(),
                digest: a.get("digest")?.as_str()?.to_owned(),
            })
        }),
        base_digest: text("base_digest"),
        backend: text("backend"),
        engine: text("engine"),
    };
    (!identity.is_empty()).then_some(identity)
}

fn parse_sse_chunk(v: &Value) -> anyhow::Result<Frame> {
    // Error frame - some OpenAI-compatible servers admit a streaming request
    // with `HTTP 200` + SSE headers, then discover mid-stream that it can't
    // actually be served (context too large, model overloaded, etc.) and
    // emit `{"error": {...}}` in place of a `choices` delta, followed by
    // `[DONE]`.  Such a frame has no `choices` for a conformant client to key
    // off, so without this check it falls all the way through to the
    // catch-all empty `TextDelta` below and the caller sees a "successful"
    // empty completion instead of the failure the server actually reported.
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("(no message)");
        match err.get("code").and_then(|c| c.as_str()) {
            Some(code) => bail!("server error ({code}): {message}"),
            None => bail!("server error: {message}"),
        }
    }

    let mut events = Vec::new();
    let mut terminal = false;
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_owned);

    // Token-level extensions that arrive on a frame of their own or beside the
    // choice: the prompt's token ids (`prompt_token_ids`, vLLM's name) and the
    // server's account of the decode (`brain`, see `served_identity`).
    if let Some(ids) = v.get("prompt_token_ids").and_then(Value::as_array) {
        events.push(ResponseEvent::PromptTokens {
            ids: ids.iter().filter_map(Value::as_i64).collect(),
        });
    }
    if let Some(identity) = v.get("brain").and_then(served_identity) {
        events.push(ResponseEvent::Identity(identity));
    }

    // Usage (emitted when stream_options.include_usage = true). Usually a
    // frame of its own with `choices: []`, but some servers attach it to the
    // last content frame, so it is read alongside the choice, never instead.
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
        // OpenRouter returns usage.cost; some providers use total_cost.
        let cost_usd = usage
            .get("cost")
            .and_then(|v| v.as_f64())
            .or_else(|| usage.get("total_cost").and_then(|v| v.as_f64()));
        events.push(ResponseEvent::Usage {
            input_tokens: fresh_input,
            output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0) as u32,
            cache_read_tokens,
            cache_write_tokens,
            cost_usd,
        });
    } else if let Some(timings) = v.get("timings") {
        // llama.cpp performance metrics (top-level `timings` object)
        // These arrive in the final SSE chunk with finish_reason=stop and provide
        // cache hit counts and generation speed that are incredibly useful for CI
        // debugging.  We convert them into a Usage event so the CI runner can emit
        // them as `[sven:tokens]` trace output.
        let cache_n = timings["cache_n"].as_u64().unwrap_or(0) as u32;
        let prompt_n = timings["prompt_n"].as_u64().unwrap_or(0) as u32;
        let predicted_n = timings["predicted_n"].as_u64().unwrap_or(0) as u32;

        // llama.cpp reports cache hits and fresh tokens separately.
        // `prompt_n` is the fresh-only count; `cache_n` goes into cache_read_tokens.
        events.push(ResponseEvent::Usage {
            input_tokens: prompt_n,
            output_tokens: predicted_n,
            cache_read_tokens: cache_n,
            cache_write_tokens: 0,
            cost_usd: None,
        });
    }

    // A frame with no `choices[0]` is only valid when it carried usage.
    // Bailing otherwise - rather than silently falling through to an empty
    // `TextDelta` - is what makes the error-frame check above actually
    // matter: any error frame that later changes shape (or any other
    // server's non-standard error encoding) would still degrade to a silent
    // "success".
    let Some(choice) = v["choices"].get(0).filter(|c| !c.is_null()) else {
        if events.is_empty() {
            bail!("unrecognised stream frame (no usage/timings/error/choices): {v}");
        }
        return Ok(Frame {
            events,
            terminal,
            model,
        });
    };

    let delta = &choice["delta"];

    // Thinking delta - two common field names for chain-of-thought reasoning:
    //   • `reasoning_content` - llama.cpp, Qwen3, DeepSeek-R1, xAI Grok-3-mini
    //   • `reasoning`         - OpenRouter (and some other aggregators)
    // Both carry the same semantics: readable CoT text that arrived before the
    // final answer.  Prefer `reasoning_content`; fall back to `reasoning`.
    let thinking_text = delta
        .get("reasoning_content")
        .and_then(|c| c.as_str())
        .or_else(|| delta.get("reasoning").and_then(|c| c.as_str()));
    if let Some(thinking) = thinking_text.filter(|t| !t.is_empty()) {
        events.push(ResponseEvent::ThinkingDelta(thinking.to_string()));
    }

    if let Some(text) = delta
        .get("content")
        .and_then(|c| c.as_str())
        .filter(|t| !t.is_empty())
    {
        events.push(ResponseEvent::TextDelta(text.to_string()));
    }

    // Token-level capture. `logprobs.content[].logprob` is the OpenAI shape;
    // `token_ids` is the extension carrying the sampled ids, accepted on the
    // choice or inside `logprobs`.
    let ids: Vec<i64> = choice
        .get("token_ids")
        .or_else(|| choice["logprobs"].get("token_ids"))
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default();
    let logprobs: Vec<f64> = choice["logprobs"]["content"]
        .as_array()
        .map(|a| a.iter().filter_map(|t| t["logprob"].as_f64()).collect())
        .unwrap_or_default();
    if !ids.is_empty() || !logprobs.is_empty() {
        events.push(ResponseEvent::Tokens { ids, logprobs });
    }

    // Tool-call deltas. One delta may carry several parallel calls, each
    // identified by its `index`; the agent routes fragments to slots by that
    // index, so every element is emitted. A server that omits `index` (it is
    // required by the schema but not always sent) is assumed to list the
    // calls in order.
    if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
        for (position, tc) in tool_calls.iter().enumerate() {
            events.push(ResponseEvent::ToolCall {
                index: tc["index"].as_u64().unwrap_or(position as u64) as u32,
                id: tc["id"].as_str().unwrap_or("").to_string(),
                name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                arguments: tool_call_arguments(&tc["function"]["arguments"]),
            });
        }
    }

    // finish_reason=length means the model hit its output-token limit.
    // Emit MaxTokens so the agent knows any pending tool-call arguments
    // are truncated.  The [DONE] sentinel that follows will emit Done.
    // finish_reason=content_filter/error means the provider stopped the
    // generation itself; surface that as a hard failure rather than letting
    // it look like a normal (if truncated) completion.
    match choice["finish_reason"].as_str() {
        Some("length") => {
            events.push(ResponseEvent::MaxTokens);
            terminal = true;
        }
        Some("content_filter") => bail!("stream stopped: content_filter"),
        Some("error") => bail!("stream stopped: error"),
        Some(_) => terminal = true,
        None => {
            // A server whose stop reason has no OpenAI name reports no
            // `finish_reason` and puts the real one in
            // `native_finish_reason`. That generation did not complete
            // normally.
            if let Some(native) = choice["native_finish_reason"].as_str() {
                bail!("stream stopped: {native}");
            }
        }
    }

    if events.is_empty() {
        events.push(ResponseEvent::TextDelta(String::new()));
    }
    Ok(Frame {
        events,
        terminal,
        model,
    })
}

/// A tool call's `arguments` as JSON text. The schema says string; a few
/// servers send the parsed object instead, which is re-serialised so every
/// downstream consumer sees one shape.
fn tool_call_arguments(arguments: &Value) -> String {
    match arguments {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
