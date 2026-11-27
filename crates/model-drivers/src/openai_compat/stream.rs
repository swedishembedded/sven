// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! SSE stream parsing for OpenAI-compatible chat completion responses.

use std::collections::VecDeque;

use anyhow::{bail, Context};
use futures::{Stream, StreamExt};
use serde_json::value::RawValue;
use serde_json::Value;

use serde::Deserialize;
use sven_model::ResponseEvent;
use sven_vocab::{
    AdapterRef, LogprobKind, PolicyLineage, SamplerSupport, ServedHistory, ServedIdentity,
};

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
        Ok(v) => parse_sse_chunk(&v).map(|mut frame| {
            let sampled = verbatim_parts(data, &mut frame.events);
            frame.events.extend(sampled);
            frame
        }),
        Err(e) => Err(anyhow::anyhow!("malformed stream frame ({e}): {data}")),
    })
}

/// The parts of a frame that must reach the runtime as the server wrote them,
/// read from its text rather than from the parsed [`Value`] (whose objects
/// lose their key order): a reply's `sampled` record (returned as an event),
/// and a call's `arguments` sent as a JSON object instead of the text the
/// schema asks for (which replaces the parsed form in its `ToolCall` event).
fn verbatim_parts(data: &str, events: &mut [ResponseEvent]) -> Option<ResponseEvent> {
    #[derive(serde::Deserialize)]
    struct Frame<'a> {
        #[serde(borrow, default)]
        choices: Vec<Choice<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Choice<'a> {
        #[serde(borrow, default)]
        delta: Option<Parts<'a>>,
        #[serde(borrow, default)]
        message: Option<Parts<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Parts<'a> {
        #[serde(borrow, default)]
        sampled: Option<&'a RawValue>,
        #[serde(borrow, default)]
        tool_calls: Vec<Call<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Call<'a> {
        #[serde(borrow, default)]
        function: Option<Function<'a>>,
    }
    #[derive(serde::Deserialize)]
    struct Function<'a> {
        #[serde(borrow, default)]
        arguments: Option<&'a RawValue>,
    }
    if !data.contains("\"sampled\"")
        && !data.contains("\"arguments\":{")
        && !data.contains("\"arguments\": {")
    {
        return None;
    }
    let frame: Frame<'_> = serde_json::from_str(data).ok()?;
    let parts = frame
        .choices
        .into_iter()
        .next()
        .and_then(|c| c.delta.or(c.message))?;
    let objects = parts.tool_calls.iter().map(|c| {
        c.function
            .as_ref()
            .and_then(|f| f.arguments)
            .filter(|a| a.get().starts_with('{'))
    });
    let mut calls = events.iter_mut().filter_map(|e| match e {
        ResponseEvent::ToolCall { arguments, .. } => Some(arguments),
        _ => None,
    });
    for raw in objects {
        let Some(arguments) = calls.next() else { break };
        if let Some(raw) = raw {
            *arguments = raw.get().to_string();
        }
    }
    parts
        .sampled
        .filter(|raw| raw.get() != "null")
        .map(|raw| ResponseEvent::Sampled(raw.to_owned()))
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
    body: impl Stream<Item = Result<B, E>> + Unpin + sven_rt::MaybeSend + 'static,
    seed: Option<u64>,
) -> impl Stream<Item = anyhow::Result<ResponseEvent>> + sven_rt::MaybeSend
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

/// What a brain server's `policy_trace` object carries: the prompt ids, the
/// generated ids (the stop token included) and the log-probability of each
/// at `log_prob_temperature`, scored over the whole vocabulary by the serving
/// model itself. They are the policy's own, so the call's identity says so
/// and at which temperature; whether that is the temperature the request
/// sampled at is for the consumer to check against what it asked for.
pub(crate) fn policy_trace_events(
    trace: &Value,
    events: &mut Vec<ResponseEvent>,
    identity: &mut ServedIdentity,
) -> anyhow::Result<()> {
    let numbers = |key: &str| trace.get(key).and_then(Value::as_array);
    if let Some(ids) = numbers("prompt_token_ids") {
        events.push(ResponseEvent::PromptTokens {
            ids: ids.iter().filter_map(Value::as_i64).collect(),
        });
    }
    events.push(ResponseEvent::Tokens {
        ids: numbers("generation_token_ids")
            .map(|a| a.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default(),
        logprobs: numbers("generation_log_probs")
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default(),
    });
    if let Some(top) = top_tokens(trace)? {
        events.push(top);
    }
    if let Some(support) = sampler_support(trace)? {
        events.push(ResponseEvent::SamplerSupport(support));
    }
    identity.logprob_kind = Some(LogprobKind::Policy);
    identity.logprob_temperature = trace.get("log_prob_temperature").and_then(Value::as_f64);
    Ok(())
}

/// The sampler's top list of a `policy_trace` (`generation_top_ids` and
/// `generation_top_log_probs`: one list of candidates per generated token),
/// when the server returned one. The two lists must be parallel to each other
/// and to `generation_token_ids`: a malformed one is a failure of the reply, not
/// a shorter record. `None` when the trace has neither (the request did not
/// ask, or the server cannot; the turn that asked fails on the absence).
fn top_tokens(trace: &Value) -> anyhow::Result<Option<ResponseEvent>> {
    fn grid<T>(
        trace: &Value,
        key: &str,
        read: fn(&Value) -> Option<T>,
    ) -> anyhow::Result<Option<Vec<Vec<T>>>> {
        let Some(list) = trace.get(key).filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let rows = list
            .as_array()
            .with_context(|| format!("policy_trace.{key} is not a list"))?;
        rows.iter()
            .map(|row| {
                row.as_array()
                    .with_context(|| format!("policy_trace.{key} holds a row that is not a list"))?
                    .iter()
                    .map(|v| {
                        read(v).with_context(|| format!("policy_trace.{key} holds a bad entry"))
                    })
                    .collect()
            })
            .collect::<anyhow::Result<Vec<_>>>()
            .map(Some)
    }
    let ids = grid(trace, "generation_top_ids", |v| v.as_i64())?;
    let logprobs = grid(trace, "generation_top_log_probs", |v| v.as_f64())?;
    let (ids, logprobs) = match (ids, logprobs) {
        (None, None) => return Ok(None),
        (Some(ids), Some(logprobs)) => (ids, logprobs),
        _ => bail!(
            "policy_trace carries only one of generation_top_ids and generation_top_log_probs"
        ),
    };
    let generated = trace
        .get("generation_token_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if ids.len() != generated || logprobs.len() != generated {
        bail!(
            "policy_trace top lists cover {} and {} positions for {generated} generated tokens",
            ids.len(),
            logprobs.len()
        );
    }
    if ids.iter().zip(&logprobs).any(|(i, l)| i.len() != l.len()) {
        bail!("policy_trace top ids and log-probabilities differ in length at a position");
    }
    Ok(Some(ResponseEvent::TopTokens { ids, logprobs }))
}

/// The sampler's support of a `policy_trace` (`support_cap`, `generation_sampler_log_probs`,
/// `generation_support_offsets` and `generation_support_ids`), when the server returned one. It is the one reader
/// of these fields for every transport: all four must be present together and consistent with each other and with
/// `generation_token_ids`, because a trainer replays the draw from them and a short list would be read as a
/// smaller support. `None` when the trace has none of them (the request did not ask, or the server cannot; the turn
/// that asked fails on the absence).
fn sampler_support(trace: &Value) -> anyhow::Result<Option<SamplerSupport>> {
    const KEYS: [&str; 4] = [
        "support_cap",
        "generation_sampler_log_probs",
        "generation_support_offsets",
        "generation_support_ids",
    ];
    let present = KEYS
        .iter()
        .filter(|key| trace.get(**key).is_some_and(|v| !v.is_null()))
        .count();
    match present {
        0 => return Ok(None),
        4 => {}
        _ => bail!("policy_trace carries only some of {}", KEYS.join(", ")),
    }
    fn list<T>(
        trace: &Value,
        key: &str,
        read: impl Fn(&Value) -> Option<T>,
    ) -> anyhow::Result<Vec<T>> {
        trace
            .get(key)
            .and_then(Value::as_array)
            .with_context(|| format!("policy_trace.{key} is not a list"))?
            .iter()
            .map(|v| read(v).with_context(|| format!("policy_trace.{key} holds a bad entry")))
            .collect()
    }
    let cap = trace
        .get("support_cap")
        .and_then(Value::as_u64)
        .and_then(|c| u32::try_from(c).ok())
        .context("policy_trace.support_cap is not a number")?;
    let support = SamplerSupport {
        cap,
        log_probs: list(trace, "generation_sampler_log_probs", |v| v.as_f64())?,
        offsets: list(trace, "generation_support_offsets", |v| {
            v.as_u64().and_then(|o| u32::try_from(o).ok())
        })?,
        ids: list(trace, "generation_support_ids", |v| v.as_i64())?,
    };
    let generated = trace
        .get("generation_token_ids")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    support
        .check(generated)
        .map_err(|why| anyhow::anyhow!("policy_trace support: {why}"))?;
    Ok(Some(support))
}

/// The server's account of a decode, from a frame's non-standard `brain`
/// object: `seed`, `adapter {id, digest, version, lineage?}`, `base_digest`,
/// `backend`, `engine`, and the keys a server may add: `policy_version` (else
/// the adapter's `version`), `tokenizer_digest`, `chat_template_digest` and
/// `logprob_kind` (`policy` or `sampler`). The adapter's `lineage` (`run`,
/// `epoch`, `update`, `base_digest`, `adapter_digest`) is kept only whole: a
/// malformed one is reported in the log and recorded as unknown, never
/// half-filled. `None` when it names none of them.
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
        policy_version: brain
            .get("policy_version")
            .or_else(|| brain.get("adapter").and_then(|a| a.get("version")))
            .and_then(|v| match v {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            }),
        policy_lineage: brain
            .get("adapter")
            .and_then(|a| a.get("lineage"))
            .filter(|l| !l.is_null())
            .and_then(|l| {
                PolicyLineage::deserialize(l)
                    .inspect_err(|e| {
                        tracing::warn!("the server's adapter lineage is not one: {e}");
                    })
                    .ok()
                    .map(Box::new)
            }),
        tokenizer_digest: text("tokenizer_digest"),
        chat_template_digest: text("chat_template_digest"),
        logprob_kind: match brain.get("logprob_kind").and_then(Value::as_str) {
            Some("policy") => Some(LogprobKind::Policy),
            Some("sampler") => Some(LogprobKind::Sampler),
            _ => None,
        },
        logprob_temperature: None,
        history: brain
            .get("history")
            .filter(|h| !h.is_null())
            .and_then(|h| ServedHistory::deserialize(h).ok()),
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
        let message = crate::server_text::printable(message);
        match err
            .get("code")
            .and_then(|c| c.as_str())
            .map(crate::server_text::printable)
        {
            Some(code) => bail!("server error ({code}): {message}"),
            None => bail!("server error: {message}"),
        }
    }

    let mut events = Vec::new();
    let mut terminal = false;
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_owned);

    // Token-level extensions that arrive on a frame of their own or beside the
    // choice: the prompt's token ids (`prompt_token_ids`, vLLM's name), the
    // server's account of the decode (`brain`, see `served_identity`) and a
    // brain server's `policy_trace` (see `policy_trace_events`).
    let first_choice = v["choices"].get(0).unwrap_or(&Value::Null);
    if let Some(ids) = field(
        &[v, first_choice, &first_choice["message"]],
        "prompt_token_ids",
    )
    .and_then(Value::as_array)
    {
        events.push(ResponseEvent::PromptTokens {
            ids: ids.iter().filter_map(Value::as_i64).collect(),
        });
    }
    let mut identity = v.get("brain").and_then(served_identity);
    if let Some(trace) = v.get("policy_trace").filter(|t| !t.is_null()) {
        policy_trace_events(trace, &mut events, identity.get_or_insert_default())?;
    }
    if let Some(identity) = identity {
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

    // Token-level capture in the OpenAI-compatible shape: the sampled ids are
    // `token_ids` (on the choice, its delta or message, or inside `logprobs`)
    // and their log-probabilities `logprobs.content[].logprob`. A brain
    // server's `policy_trace` is read from the frame instead (see above).
    let places = [choice, delta, &choice["message"], &choice["logprobs"]];
    let ids: Vec<i64> = field(&places, "token_ids")
        .and_then(Value::as_array)
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

/// The first non-null `key` among `places`.
fn field<'a>(places: &[&'a Value], key: &str) -> Option<&'a Value> {
    places
        .iter()
        .find_map(|place| place.get(key).filter(|value| !value.is_null()))
}

/// A tool call's `arguments` as JSON text. The schema says string; a server
/// that sends the object instead has its text put back by [`verbatim_parts`].
fn tool_call_arguments(arguments: &Value) -> String {
    match arguments {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod verbatim_tests {
    use super::*;

    fn events(line: &str) -> Vec<ResponseEvent> {
        parse_sse_data_line(line).unwrap().unwrap().events
    }

    /// A reply's `sampled` record arrives as the exact text the server sent,
    /// key order and spacing included.
    #[test]
    fn a_sampled_record_is_kept_as_the_server_wrote_it() {
        let raw = r#"{"token_ids":"AQID","finish":"stop","context_len":3,"context_hash":"sha256:ab", "prefill_token_ids":"BA=="}"#;
        let line = format!(
            r#"data: {{"choices":[{{"index":0,"delta":{{"sampled":{raw}}},"finish_reason":"stop"}}]}}"#
        );
        let got: Vec<String> = events(&line)
            .into_iter()
            .filter_map(|e| match e {
                ResponseEvent::Sampled(r) => Some(r.get().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(got, vec![raw.to_string()]);
    }

    /// A server that sends a call's arguments as an object has them taken as
    /// the text it wrote, not re-serialised.
    #[test]
    fn arguments_sent_as_an_object_keep_their_text() {
        let line = r#"data: {"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":{"z": 1, "a": 2.50}}}]}}]}"#;
        let args: Vec<String> = events(line)
            .into_iter()
            .filter_map(|e| match e {
                ResponseEvent::ToolCall { arguments, .. } => Some(arguments),
                _ => None,
            })
            .collect();
        assert_eq!(args, vec![r#"{"z": 1, "a": 2.50}"#.to_string()]);
    }
}
