// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for dependable agent-to-model
// integration for its clients. If your team needs expertise in OpenAI-compatible
// streaming protocols or tool-calling agents then you can procure our services
// by sending an email to info@swedishembedded.com.
//! The OpenAI-compatible streaming contract, as a client must read it.
//!
//! Every test serves a hand-written wire transcript from a loopback socket and
//! drives the public `openai` provider against it, so the SSE framing, the
//! chunked transfer and the event mapping are exercised together. The
//! transcripts are what conformant hosted and self-hosted servers put on the
//! wire; none of them is shaped to suit this client.
//!
//! Covered: text deltas, tool-call fragments, several tool calls in one delta
//! array and interleaved across deltas by index, arguments split across
//! chunks, finish reasons, output-token truncation, mid-stream error frames,
//! usage frames, an interrupted connection, model listing and context-size
//! discovery.

mod common;

use std::time::Duration;

use futures::StreamExt;
use serde_json::{json, Value};
use sven_model::{CompletionRequest, Message, ModelProvider, ResponseEvent};
use sven_model_drivers::{from_config, ModelConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ── Loopback server ───────────────────────────────────────────────────────────

/// How a scripted response body ends.
#[derive(Clone, Copy)]
enum Ending {
    /// The terminating zero-length chunk is sent: a complete HTTP body.
    Complete,
    /// The socket is closed mid-body, with no terminating chunk.
    Abort,
}

/// One scripted answer: chunked transfer, one HTTP chunk per `segments`
/// element, so a test controls exactly where the byte stream is cut.
struct Script {
    segments: Vec<Vec<u8>>,
    ending: Ending,
}

/// Serve `chat` for `POST .../chat/completions` and `models` for
/// `GET .../models`; everything else is a 404. Returns the base URL.
async fn serve(chat: Option<Script>, models: Option<Value>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let chat = std::sync::Arc::new(std::sync::Mutex::new(chat));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let chat = std::sync::Arc::clone(&chat);
            let models = models.clone();
            tokio::spawn(async move {
                let head = read_request(&mut sock).await;
                let line = head.lines().next().unwrap_or_default().to_string();
                if line.starts_with("POST") && line.contains("/chat/completions") {
                    let script = chat.lock().unwrap().take();
                    match script {
                        Some(s) => write_chunked(&mut sock, s).await,
                        None => write_plain(&mut sock, 404, "{}").await,
                    }
                } else if line.starts_with("GET") && line.contains("/v1/models") {
                    match models {
                        Some(m) => write_plain(&mut sock, 200, &m.to_string()).await,
                        None => write_plain(&mut sock, 404, "{}").await,
                    }
                } else {
                    write_plain(&mut sock, 404, "{}").await;
                }
            });
        }
    });
    base
}

/// Read one HTTP request (head and any body) and return the head.
async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = sock.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        if let Some(end) = text.find("\r\n\r\n") {
            let len = text
                .lines()
                .find_map(|l| {
                    l.to_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_string)
                })
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= end + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

async fn write_plain(sock: &mut tokio::net::TcpStream, status: u16, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(resp.as_bytes()).await;
}

async fn write_chunked(sock: &mut tokio::net::TcpStream, script: Script) {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    let _ = sock.write_all(head.as_bytes()).await;
    for seg in &script.segments {
        let mut frame = format!("{:x}\r\n", seg.len()).into_bytes();
        frame.extend_from_slice(seg);
        frame.extend_from_slice(b"\r\n");
        let _ = sock.write_all(&frame).await;
        let _ = sock.flush().await;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    if matches!(script.ending, Ending::Complete) {
        let _ = sock.write_all(b"0\r\n\r\n").await;
    }
    let _ = sock.shutdown().await;
}

/// SSE `data:` frame for a JSON value.
fn frame(v: Value) -> Vec<u8> {
    format!("data: {v}\n\n").into_bytes()
}

fn done() -> Vec<u8> {
    b"data: [DONE]\n\n".to_vec()
}

fn complete(segments: Vec<Vec<u8>>) -> Script {
    Script {
        segments,
        ending: Ending::Complete,
    }
}

/// A chunk carrying one choice delta and an optional finish reason.
fn choice(delta: Value, finish: Value) -> Value {
    json!({"id": "c1", "object": "chat.completion.chunk", "model": "m",
           "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
}

fn tool_delta(index: u32, id: Option<&str>, name: Option<&str>, args: &str) -> Value {
    let mut tc = json!({"index": index, "function": {"arguments": args}});
    if let Some(id) = id {
        tc["id"] = json!(id);
        tc["type"] = json!("function");
    }
    if let Some(name) = name {
        tc["function"]["name"] = json!(name);
    }
    tc
}

fn provider(base: &str) -> Box<dyn ModelProvider> {
    from_config(&ModelConfig {
        provider: "openai".into(),
        name: "m".into(),
        api_key: Some("k".into()),
        base_url: Some(base.to_string()),
        ..ModelConfig::default()
    })
    .unwrap()
}

/// Run one streaming completion to the end of the byte stream.
async fn run(script: Script) -> Vec<anyhow::Result<ResponseEvent>> {
    let base = serve(Some(script), None).await;
    let mut stream = provider(&base)
        .complete(CompletionRequest {
            messages: vec![Message::user("go")],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(ev) = stream.next().await {
        out.push(ev);
    }
    out
}

fn ok_events(events: Vec<anyhow::Result<ResponseEvent>>) -> Vec<ResponseEvent> {
    events
        .into_iter()
        .map(|e| e.expect("no stream error"))
        .collect()
}

/// What an agent reassembles from `ToolCall` events: per index, the first
/// non-empty id and name and the concatenated argument text.
fn assemble(events: &[ResponseEvent]) -> Vec<(u32, String, String, String)> {
    let mut slots: Vec<(u32, String, String, String)> = Vec::new();
    for ev in events {
        if let ResponseEvent::ToolCall {
            index,
            id,
            name,
            arguments,
        } = ev
        {
            let pos = match slots.iter().position(|s| s.0 == *index) {
                Some(p) => p,
                None => {
                    slots.push((*index, String::new(), String::new(), String::new()));
                    slots.len() - 1
                }
            };
            if slots[pos].1.is_empty() {
                slots[pos].1 = id.clone();
            }
            if slots[pos].2.is_empty() {
                slots[pos].2 = name.clone();
            }
            slots[pos].3.push_str(arguments);
        }
    }
    slots.sort_by_key(|s| s.0);
    slots
}

fn text_of(events: &[ResponseEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::TextDelta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

// ── Text ──────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn text_deltas_arrive_in_order_and_the_stream_ends_with_done() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"role": "assistant", "content": ""}),
                Value::Null,
            )),
            frame(choice(json!({"content": "Hel"}), Value::Null)),
            frame(choice(json!({"content": "lo"}), Value::Null)),
            frame(choice(json!({}), json!("stop"))),
            done(),
        ]))
        .await,
    );
    assert_eq!(text_of(&events), "Hello");
    assert!(matches!(events.last(), Some(ResponseEvent::Done)));
}

#[tokio::test]
async fn a_data_field_without_the_optional_space_is_still_a_frame() {
    // The SSE grammar allows `data:{...}`; a server may legitimately omit
    // the space.
    let bare = format!("data:{}\n\n", choice(json!({"content": "hi"}), Value::Null)).into_bytes();
    let events = ok_events(run(complete(vec![bare, done()])).await);
    assert_eq!(text_of(&events), "hi");
}

#[tokio::test]
async fn a_frame_split_across_http_chunks_is_reassembled() {
    let whole = frame(choice(json!({"content": "split"}), Value::Null));
    let (a, b) = whole.split_at(whole.len() / 2);
    let events = ok_events(run(complete(vec![a.to_vec(), b.to_vec(), done()])).await);
    assert_eq!(text_of(&events), "split");
}

#[tokio::test]
async fn reasoning_text_is_kept_apart_from_the_answer() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(json!({"reasoning_content": "think"}), Value::Null)),
            frame(choice(json!({"content": "answer"}), Value::Null)),
            frame(choice(json!({}), json!("stop"))),
            done(),
        ]))
        .await,
    );
    assert_eq!(text_of(&events), "answer");
    assert!(events
        .iter()
        .any(|e| matches!(e, ResponseEvent::ThinkingDelta(t) if t == "think")));
}

// ── Tool calls ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn one_tool_call_streamed_as_fragments_is_reassembled() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"role": "assistant", "content": null,
                       "tool_calls": [tool_delta(0, Some("call_a"), Some("read_file"), "")]}),
                Value::Null,
            )),
            frame(choice(
                json!({"tool_calls": [tool_delta(0, None, None, "{\"path\":")]}),
                Value::Null,
            )),
            frame(choice(
                json!({"tool_calls": [tool_delta(0, None, None, "\"src/lib.rs\"}")]}),
                Value::Null,
            )),
            frame(choice(json!({}), json!("tool_calls"))),
            done(),
        ]))
        .await,
    );
    let calls = assemble(&events);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, "call_a");
    assert_eq!(calls[0].2, "read_file");
    let args: Value = serde_json::from_str(&calls[0].3).expect("arguments are whole JSON");
    assert_eq!(args, json!({"path": "src/lib.rs"}));
}

#[tokio::test]
async fn several_tool_calls_in_one_delta_array_are_all_delivered() {
    // One chunk, two calls: both must reach the agent, not only the first.
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"tool_calls": [
                    tool_delta(0, Some("call_a"), Some("read_file"), "{\"path\":\"a\"}"),
                    tool_delta(1, Some("call_b"), Some("read_file"), "{\"path\":\"b\"}"),
                    tool_delta(2, Some("call_c"), Some("list_dir"), "{}"),
                ]}),
                Value::Null,
            )),
            frame(choice(json!({}), json!("tool_calls"))),
            done(),
        ]))
        .await,
    );
    let calls = assemble(&events);
    let names: Vec<_> = calls
        .iter()
        .map(|c| (c.1.as_str(), c.2.as_str(), c.3.as_str()))
        .collect();
    assert_eq!(
        names,
        vec![
            ("call_a", "read_file", "{\"path\":\"a\"}"),
            ("call_b", "read_file", "{\"path\":\"b\"}"),
            ("call_c", "list_dir", "{}"),
        ]
    );
}

#[tokio::test]
async fn parallel_tool_calls_interleaved_across_deltas_stay_apart_by_index() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"tool_calls": [
                    tool_delta(0, Some("call_a"), Some("fs"), "{\"p\":"),
                    tool_delta(1, Some("call_b"), Some("shell"), "{\"cmd\":"),
                ]}),
                Value::Null,
            )),
            frame(choice(
                json!({"tool_calls": [tool_delta(1, None, None, "\"ls\"}")]}),
                Value::Null,
            )),
            frame(choice(
                json!({"tool_calls": [
                    tool_delta(0, None, None, "1}"),
                ]}),
                Value::Null,
            )),
            frame(choice(json!({}), json!("tool_calls"))),
            done(),
        ]))
        .await,
    );
    let calls = assemble(&events);
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (
            calls[0].1.as_str(),
            calls[0].2.as_str(),
            calls[0].3.as_str()
        ),
        ("call_a", "fs", "{\"p\":1}")
    );
    assert_eq!(
        (
            calls[1].1.as_str(),
            calls[1].2.as_str(),
            calls[1].3.as_str()
        ),
        ("call_b", "shell", "{\"cmd\":\"ls\"}")
    );
}

#[tokio::test]
async fn text_and_a_tool_call_in_the_same_delta_both_survive() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"content": "Looking.",
                       "tool_calls": [tool_delta(0, Some("call_a"), Some("fs"), "{}")]}),
                Value::Null,
            )),
            frame(choice(json!({}), json!("tool_calls"))),
            done(),
        ]))
        .await,
    );
    assert_eq!(text_of(&events), "Looking.");
    assert_eq!(assemble(&events).len(), 1);
}

#[tokio::test]
async fn arguments_sent_as_a_json_object_are_delivered_as_json_text() {
    // Some servers send `arguments` as an object rather than a string.
    let tc = json!({"index": 0, "id": "call_a", "type": "function",
                    "function": {"name": "fs", "arguments": {"path": "x"}}});
    let events = ok_events(
        run(complete(vec![
            frame(choice(json!({"tool_calls": [tc]}), Value::Null)),
            frame(choice(json!({}), json!("tool_calls"))),
            done(),
        ]))
        .await,
    );
    let calls = assemble(&events);
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].3).unwrap(),
        json!({"path": "x"})
    );
}

// ── Finish reasons and truncation ─────────────────────────────────────────────

#[tokio::test]
async fn output_token_truncation_is_reported_as_max_tokens() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(
                json!({"tool_calls": [tool_delta(0, Some("call_a"), Some("fs"), "{\"path\":\"a")]}),
                Value::Null,
            )),
            frame(choice(json!({}), json!("length"))),
            done(),
        ]))
        .await,
    );
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::MaxTokens)));
}

#[tokio::test]
async fn content_in_the_frame_that_finishes_with_length_is_not_lost() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(json!({"content": "tail"}), json!("length"))),
            done(),
        ]))
        .await,
    );
    assert_eq!(text_of(&events), "tail");
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::MaxTokens)));
}

#[tokio::test]
async fn a_content_filter_stop_is_a_failure_not_a_short_answer() {
    let events = run(complete(vec![
        frame(choice(json!({"content": "x"}), Value::Null)),
        frame(choice(json!({}), json!("content_filter"))),
        done(),
    ]))
    .await;
    assert!(events.iter().any(Result::is_err));
}

#[tokio::test]
async fn a_generation_that_ended_without_a_reason_is_a_failure() {
    // A server that cannot say why it stopped (null finish reason, the real
    // reason only in `native_finish_reason`) delivered a partial answer.
    let mut last = choice(json!({}), Value::Null);
    last["choices"][0]["native_finish_reason"] = json!("cancelled");
    let events = run(complete(vec![
        frame(choice(json!({"content": "half"}), Value::Null)),
        frame(last),
        done(),
    ]))
    .await;
    assert!(events.iter().any(Result::is_err));
}

// ── Errors, usage, interruption ───────────────────────────────────────────────

#[tokio::test]
async fn an_error_frame_after_content_surfaces_after_the_content() {
    let events = run(complete(vec![
        frame(choice(json!({"content": "partial"}), Value::Null)),
        frame(json!({"error": {"message": "engine failed", "type": "server_error", "code": "internal_error"}})),
        done(),
    ]))
    .await;
    assert!(matches!(&events[0], Ok(ResponseEvent::TextDelta(t)) if t == "partial"));
    let err = events
        .iter()
        .find_map(|e| e.as_ref().err())
        .expect("an error event");
    let msg = err.to_string();
    assert!(
        msg.contains("engine failed") && msg.contains("internal_error"),
        "{msg}"
    );
}

#[tokio::test]
async fn a_usage_only_frame_becomes_a_usage_event() {
    let events = ok_events(
        run(complete(vec![
            frame(choice(json!({"content": "ok"}), json!("stop"))),
            frame(json!({"id": "c1", "object": "chat.completion.chunk", "model": "m",
                         "choices": [],
                         "usage": {"prompt_tokens": 12, "completion_tokens": 5, "total_tokens": 17}})),
            done(),
        ]))
        .await,
    );
    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::Usage {
            input_tokens: 12,
            output_tokens: 5,
            ..
        }
    )));
}

#[tokio::test]
async fn usage_riding_on_the_last_choice_frame_keeps_the_choice_content() {
    // Several servers attach `usage` to the final content frame instead of
    // sending a separate usage-only frame.
    let mut last = choice(json!({"content": "end"}), json!("stop"));
    last["usage"] = json!({"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4});
    let events = ok_events(run(complete(vec![frame(last), done()])).await);
    assert_eq!(text_of(&events), "end");
    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::Usage {
            input_tokens: 3,
            output_tokens: 1,
            ..
        }
    )));
}

#[tokio::test]
async fn a_connection_closed_before_the_stream_finished_is_a_failure() {
    // Clean close, no finish reason, no [DONE]: the answer is cut short and
    // must not be taken for a finished one.
    let events = run(complete(vec![frame(choice(
        json!({"content": "cut"}),
        Value::Null,
    ))]))
    .await;
    assert!(events.iter().any(Result::is_err), "silent truncation");
}

#[tokio::test]
async fn a_connection_dropped_mid_body_is_a_failure() {
    let events = run(Script {
        segments: vec![frame(choice(json!({"content": "cut"}), Value::Null))],
        ending: Ending::Abort,
    })
    .await;
    assert!(events.iter().any(Result::is_err), "silent truncation");
}

#[tokio::test]
async fn a_stream_that_finished_without_done_is_accepted() {
    // A finish reason is a complete answer even when the server omits the
    // `[DONE]` sentinel.
    let events = ok_events(
        run(complete(vec![
            frame(choice(json!({"content": "whole"}), Value::Null)),
            frame(choice(json!({}), json!("stop"))),
        ]))
        .await,
    );
    assert_eq!(text_of(&events), "whole");
}

// ── Model listing and context-size discovery ──────────────────────────────────

fn model_list() -> Value {
    json!({"object": "list", "data": [
        {"id": "other/model", "object": "model", "owned_by": "node", "context_window": 8192},
        {"id": "m", "object": "model", "owned_by": "node", "context_window": 4096},
    ]})
}

#[tokio::test]
async fn the_served_models_are_listed() {
    common::isolate_model_cache();
    let base = serve(None, Some(model_list())).await;
    let models = provider(&base).list_models().await.unwrap();
    let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&"m") && ids.contains(&"other/model"),
        "{ids:?}"
    );
}

/// A hosted OpenAI-compatible provider other than OpenRouter, listed through
/// `provider`, that serves a window, an output cap and a price.
fn priced_list() -> Value {
    json!({"data": [{
        "id": "listed-priced",
        "context_length": 65536,
        "top_provider": {"max_completion_tokens": 8192},
        "pricing": {"prompt": "0.0000005", "completion": "0.0000015"}
    }, {"id": "listed-bare"}]})
}

fn provider_named(name: &str, base: &str) -> Box<dyn ModelProvider> {
    from_config(&ModelConfig {
        provider: name.into(),
        name: "listed-priced".into(),
        api_key: Some("k".into()),
        base_url: Some(base.to_string()),
        ..ModelConfig::default()
    })
    .unwrap()
}

#[tokio::test]
async fn a_successful_listing_feeds_the_catalog_for_any_openai_compatible_provider() {
    common::isolate_model_cache();
    let base = serve(None, Some(priced_list())).await;
    let groq = provider_named("groq", &base);
    groq.list_models().await.unwrap();

    let priced = sven_model::catalog::lookup("groq", "listed-priced").unwrap();
    assert_eq!(priced.context_window, Some(65_536));
    assert_eq!(priced.max_output_tokens, Some(8_192));
    let price = priced.pricing.unwrap();
    assert_eq!((price.input_per_mtok, price.output_per_mtok), (0.5, 1.5));
    assert_eq!(groq.catalog_context_window(), Some(65_536));

    let bare = sven_model::catalog::lookup("groq", "listed-bare").unwrap();
    assert_eq!((bare.context_window, bare.pricing), (None, None));
}

#[tokio::test]
async fn a_failed_listing_is_an_error_and_leaves_the_catalog_as_it_was() {
    common::isolate_model_cache();
    let ok = serve(None, Some(priced_list())).await;
    provider_named("cerebras", &ok).list_models().await.unwrap();

    let refusing = serve(None, None).await;
    let result = provider_named("cerebras", &refusing).list_models().await;
    assert!(result.is_err(), "no invented list in place of the answer");
    assert!(sven_model::catalog::lookup("cerebras", "listed-priced").is_some());
}

#[tokio::test]
async fn the_context_size_is_read_from_the_models_list_by_model_id() {
    // `context_window` is what hosted OpenAI-compatible endpoints other than
    // OpenRouter report; the model asked about, not the first one, decides.
    let base = serve(None, Some(model_list())).await;
    assert_eq!(provider(&base).probe_context_window().await, Some(4096));
}

#[tokio::test]
async fn context_length_and_max_model_len_are_still_understood() {
    for field in ["context_length", "max_model_len"] {
        let list = json!({"data": [{"id": "m", field: 2048}]});
        let base = serve(None, Some(list)).await;
        assert_eq!(
            provider(&base).probe_context_window().await,
            Some(2048),
            "{field}"
        );
    }
}
