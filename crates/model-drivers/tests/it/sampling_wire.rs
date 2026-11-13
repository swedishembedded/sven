// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reproducible, training-grade
// agent runs for its clients. If your team needs expertise in token-level run
// capture then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Sampling controls and token-level capture on the OpenAI-compatible wire:
//! what the driver sends for `CompletionRequest::sampling` / `want_logprobs`
//! and the output limit, and which `Tokens` / `Served` events it derives from
//! the reply.

use futures::StreamExt;
use serde_json::Value;
use sven_model::{CompletionRequest, Message, ResponseEvent};
use sven_model_drivers::from_config;
use sven_model_drivers::ModelConfig;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

// ── Minimal HTTP/1.1 mock server ──────────────────────────────────────────────

struct CapturedRequest {
    body: Value,
}

/// Bind a one-shot HTTP/1.1 mock server on a random loopback port.
/// It accepts exactly one request, captures it, and replies with the given
/// status + body.  Returns the port number and a receiver for the captured
/// request (fulfilled once the request has been fully read).
async fn mock_server_once(
    status: u16,
    content_type: &'static str,
    resp_body: impl Into<String> + Send + 'static,
) -> (u16, tokio::sync::oneshot::Receiver<CapturedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<CapturedRequest>();

    tokio::spawn(async move {
        let resp_body: String = resp_body.into();
        let (stream, _) = listener.accept().await.expect("accept");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // Request line, then headers (only the body length matters here).
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await.unwrap();
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(": ") {
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.parse().unwrap_or(0);
                }
            }
        }

        // Body
        let mut body_bytes = vec![0u8; content_length];
        reader.read_exact(&mut body_bytes).await.unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

        let _ = tx.send(CapturedRequest { body });

        // Write response - Content-Length so reqwest knows when to stop
        let http_resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            resp_body.len(),
            resp_body,
        );
        let _ = write_half.write_all(http_resp.as_bytes()).await;
    });

    (port, rx)
}

/// Build a minimal SSE response body from a list of `data:` payloads.
/// Appends `data: [DONE]\n\n` automatically.
fn sse_body(events: &[&str]) -> String {
    let mut s = events
        .iter()
        .map(|e| format!("data: {e}\n\n"))
        .collect::<String>();
    s.push_str("data: [DONE]\n\n");
    s
}

// ── Sampling and token-level capture (OpenAI-compatible) ──────────────────────

fn sampled_request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message::user("hello")],
        stream: true,
        sampling: sven_model::SamplingParams {
            temperature: Some(0.25),
            top_p: Some(0.9),
            top_k: Some(40),
            seed: Some(7),
        },
        want_logprobs: true,
        ..Default::default()
    }
}

#[tokio::test]
async fn openai_compat_sends_the_request_sampling_and_logprobs_flag() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        temperature: Some(0.9),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(sampled_request()).await.unwrap();
    while stream.next().await.is_some() {}

    let body = req_rx.await.unwrap().body;
    assert_eq!(body["temperature"].as_f64(), Some(0.25), "request wins");
    assert_eq!(body["top_p"].as_f64(), Some(0.9));
    assert_eq!(body["top_k"], 40);
    assert_eq!(body["seed"], 7);
    assert_eq!(body["logprobs"], true);
    assert_eq!(
        body["return_token_ids"], true,
        "the ids are asked for with the logprobs"
    );
}

#[tokio::test]
async fn openai_compat_omits_sampling_fields_it_was_not_given() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hello")],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let body = req_rx.await.unwrap().body;
    // Unset, the model's own default applies: no temperature either.
    for key in [
        "temperature",
        "top_p",
        "top_k",
        "seed",
        "logprobs",
        "return_token_ids",
    ] {
        assert!(body.get(key).is_none(), "{key} must be absent: {body}");
    }
}

#[tokio::test]
async fn openai_compat_parses_the_prompt_ids_and_the_servers_account_of_the_decode() {
    let sse = sse_body(&[
        r#"{"model":"served-7b","choices":[{"delta":{"content":"hi"}}]}"#,
        r#"{"model":"served-7b","prompt_token_ids":[5,6,7],"brain":{"seed":18446744073709551615,"adapter":{"id":"tuned","digest":"sha256:aa"},"base_digest":"sha256:bb","backend":"cuda","engine":"brain 2.0.0"},"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ]);
    let (port, _req_rx) = mock_server_once(200, "text/event-stream", sse).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(sampled_request()).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }
    let prompt: Vec<i64> = events
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::PromptTokens { ids } => Some(ids.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(prompt, vec![5, 6, 7]);
    let identity = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Identity(i) => Some(i.clone()),
            _ => None,
        })
        .expect("the identity event");
    assert_eq!(identity.seed, Some(u64::MAX), "a full-range seed survives");
    assert_eq!(
        identity.adapter.as_ref().map(|a| a.id.as_str()),
        Some("tuned")
    );
    assert_eq!(identity.base_digest.as_deref(), Some("sha256:bb"));
    assert_eq!(identity.backend.as_deref(), Some("cuda"));
    assert_eq!(identity.engine.as_deref(), Some("brain 2.0.0"));
}

#[tokio::test]
async fn openai_compat_parses_token_ids_logprobs_and_the_served_model() {
    let sse = sse_body(&[
        r#"{"model":"served-7b","choices":[{"delta":{"role":"assistant","content":"he"},"token_ids":[11,12],"logprobs":{"content":[{"token":"h","logprob":-0.5},{"token":"e","logprob":-0.25}]}}]}"#,
        r#"{"model":"served-7b","choices":[{"delta":{"content":"y"},"token_ids":[13],"logprobs":{"content":[{"token":"y","logprob":-1.0}]}}]}"#,
    ]);
    let (port, _req_rx) = mock_server_once(200, "text/event-stream", sse).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(sampled_request()).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }

    let served: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::Served { model, seed } => Some((model.clone(), *seed)),
            _ => None,
        })
        .collect();
    assert_eq!(served, vec![("served-7b".to_string(), Some(7))], "once");

    let (mut ids, mut lps): (Vec<i64>, Vec<f64>) = (Vec::new(), Vec::new());
    for e in &events {
        if let ResponseEvent::Tokens {
            ids: i,
            logprobs: l,
        } = e
        {
            ids.extend(i);
            lps.extend(l);
        }
    }
    assert_eq!(ids, vec![11, 12, 13]);
    assert_eq!(lps, vec![-0.5, -0.25, -1.0]);
}
/// Everything the provider returned of one streamed call, in order.
async fn events_of(frames: &[&str]) -> Vec<ResponseEvent> {
    let (port, _req_rx) = mock_server_once(200, "text/event-stream", sse_body(frames)).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(sampled_request()).await.unwrap();
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev.unwrap());
    }
    events
}

/// A brain server asked for the training capture answers with a
/// `policy_trace` object on the terminal chunk: the prompt ids, the generated
/// ids (the stop token included) and their log-probabilities at the scoring
/// temperature. They are read as the sampled ids and log-probabilities, with
/// the identity saying they are the policy's and at which temperature.
#[tokio::test]
async fn openai_compat_reads_a_brain_policy_trace() {
    let events = events_of(&[
        r#"{"model":"m","choices":[{"delta":{"content":"a"}}]}"#,
        r#"{"model":"m","brain":{"seed":3,"adapter":{"id":"tuned","digest":"sha256:aa","version":4}},"policy_trace":{"prompt_token_ids":[1,2,3],"generation_token_ids":[21,22,151645],"generation_log_probs":[-0.5,-0.75,-0.01],"log_prob_temperature":1.0},"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
    ])
    .await;
    let (mut ids, mut lps, mut prompt): (Vec<i64>, Vec<f64>, Vec<i64>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut identity = None;
    for e in &events {
        match e {
            ResponseEvent::Tokens {
                ids: i,
                logprobs: l,
            } => {
                ids.extend(i);
                lps.extend(l);
            }
            ResponseEvent::PromptTokens { ids } => prompt.extend(ids),
            ResponseEvent::Identity(i) => identity = Some(i.clone()),
            _ => {}
        }
    }
    assert_eq!(ids, vec![21, 22, 151645]);
    assert_eq!(lps, vec![-0.5, -0.75, -0.01]);
    assert_eq!(prompt, vec![1, 2, 3]);
    let identity = identity.expect("one identity event");
    assert_eq!(identity.logprob_kind, Some(sven_vocab::LogprobKind::Policy));
    assert_eq!(identity.logprob_temperature, Some(1.0));
    // The adapter's release is the policy version.
    assert_eq!(identity.policy_version.as_deref(), Some("4"));
    assert_eq!(identity.seed, Some(3));
}

/// A request for the capture asks a brain server for the policy trace and
/// nothing else of the capture; any other server gets the OpenAI shape.
#[tokio::test]
async fn the_capture_is_asked_of_a_brain_server_as_a_policy_trace() {
    let (port, req_rx) = mock_server_once(
        200,
        "text/event-stream",
        sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]),
    )
    .await;
    let cfg = ModelConfig {
        provider: "brain".into(),
        name: "m".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(sampled_request()).await.unwrap();
    while stream.next().await.is_some() {}
    let body = req_rx.await.unwrap().body;
    assert_eq!(body["policy_trace"], true);
    assert!(body.get("logprobs").is_none() && body.get("return_token_ids").is_none());
}

/// The identity a training server reports beside the capture: the policy
/// version, the tokenizer and chat template digests and what its
/// log-probabilities are.
#[tokio::test]
async fn openai_compat_reads_the_policy_identity() {
    let events = events_of(&[
        r#"{"model":"m","brain":{"policy_version":"v12","tokenizer_digest":"sha256:tt","chat_template_digest":"sha256:cc","logprob_kind":"policy"},"choices":[{"delta":{"content":"a"}}]}"#,
    ])
    .await;
    let identity = events
        .iter()
        .find_map(|e| match e {
            ResponseEvent::Identity(i) => Some(i.clone()),
            _ => None,
        })
        .expect("the identity event");
    assert_eq!(identity.policy_version.as_deref(), Some("v12"));
    assert_eq!(identity.tokenizer_digest.as_deref(), Some("sha256:tt"));
    assert_eq!(identity.chat_template_digest.as_deref(), Some("sha256:cc"));
    assert_eq!(identity.logprob_kind, Some(sven_vocab::LogprobKind::Policy));
}

/// The room a prompt leaves of the window is sven's estimate; an
/// OpenAI-compatible server bounds an unlimited reply by its context exactly,
/// so the request carries no limit then. A limit that was set is sent.
#[tokio::test]
async fn openai_compat_sends_no_limit_when_the_budget_is_only_the_room_left() {
    for (is_room, expect) in [(true, None), (false, Some(9000))] {
        let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]);
        let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;
        let cfg = ModelConfig {
            provider: "openai".into(),
            name: "gpt-4o-mini".into(),
            api_key: Some("sk-test".into()),
            base_url: Some(format!("http://127.0.0.1:{port}/v1")),
            ..ModelConfig::default()
        };
        let provider = from_config(&cfg).unwrap();
        let mut stream = provider
            .complete(CompletionRequest {
                messages: vec![Message::user("hello")],
                stream: true,
                max_output_tokens_override: Some(9000),
                output_limit_is_room: is_room,
                ..Default::default()
            })
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let body = req_rx.await.unwrap().body;
        assert_eq!(
            body.get("max_completion_tokens").and_then(|v| v.as_u64()),
            expect,
            "{body}"
        );
        assert!(body.get("max_tokens").is_none(), "{body}");
    }
}

// ── Thinking budget (server-side budget forcing) ──────────────────────────────

async fn body_for(base_url: impl Fn(u16) -> String, request: CompletionRequest) -> Value {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "local-model".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(base_url(port)),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(request).await.unwrap();
    while stream.next().await.is_some() {}
    req_rx.await.unwrap().body
}

#[tokio::test]
async fn a_local_server_is_asked_to_end_the_thinking_at_the_budget() {
    // A server that forces the end of the think block at the budget makes the
    // model answer; a client that only aborts at the budget throws the whole
    // turn away. The thinking switch goes along too, whatever the driver's name.
    let request = CompletionRequest {
        messages: vec![Message::user("hello")],
        stream: true,
        enable_thinking: Some(true),
        thinking_budget: Some(8000),
        ..Default::default()
    };
    let body = body_for(|port| format!("http://127.0.0.1:{port}/v1"), request).await;
    assert_eq!(body["thinking_budget"], 8000, "{body}");
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"], true,
        "{body}"
    );
}

#[tokio::test]
async fn no_thinking_budget_is_sent_when_none_is_set() {
    let request = CompletionRequest {
        messages: vec![Message::user("hello")],
        stream: true,
        ..Default::default()
    };
    let body = body_for(|port| format!("http://127.0.0.1:{port}/v1"), request).await;
    assert!(body.get("thinking_budget").is_none(), "{body}");
}
