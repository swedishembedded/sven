// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working inside a small
// model context window for its clients. If your team needs expertise in
// prompt caching for agent loops then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `tool_choice: none` on the OpenAI-compatible wire: a request may offer its
//! tools without letting the model call one, which keeps the prompt identical
//! to the requests that do use them.

use futures::StreamExt;
use serde_json::Value;
use sven_model::{CompletionRequest, Message, ToolChoice, ToolSchema};
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

fn request(tool_choice: ToolChoice) -> CompletionRequest {
    CompletionRequest {
        messages: vec![Message::user("hello")],
        tools: vec![ToolSchema {
            name: "shell".into(),
            description: "Runs a command.".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            is_mcp: false,
        }],
        stream: true,
        tool_choice,
        ..Default::default()
    }
}

async fn body_sent(tool_choice: ToolChoice) -> Value {
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
    assert!(provider.supports_tool_choice_none());
    let mut stream = provider.complete(request(tool_choice)).await.unwrap();
    while stream.next().await.is_some() {}
    req_rx.await.unwrap().body
}

#[tokio::test]
async fn tools_are_offered_with_tool_choice_none_when_asked() {
    let body = body_sent(ToolChoice::None).await;
    assert_eq!(body["tool_choice"], "none");
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn the_default_sends_no_tool_choice() {
    let body = body_sent(ToolChoice::Auto).await;
    assert!(body.get("tool_choice").is_none(), "{body}");
    assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
}
