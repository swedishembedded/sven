// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Wire-format tests: spin up a minimal HTTP/1.1 mock server, configure each
//! driver to point at it, issue a `CompletionRequest`, and assert both the
//! HTTP request the driver sent and the `ResponseEvent`s it emitted.
//!
//! These tests run without any API keys and without external network access.
//! They exercise the full driver pipeline: serialisation → HTTP → SSE parsing.

use futures::StreamExt;
use serde_json::Value;
use std::collections::HashMap;
use sven_config::ModelConfig;
use sven_model::{from_config, CompletionRequest, ContentPart, Message, ResponseEvent, ToolSchema};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

// ── Minimal HTTP/1.1 mock server ──────────────────────────────────────────────

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
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

        // Request line
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await.unwrap();
        let request_line = request_line.trim().to_string();
        let mut parts = request_line.splitn(3, ' ');
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        // Headers
        let mut headers: HashMap<String, String> = HashMap::new();
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some((k, v)) = trimmed.split_once(": ") {
                let key = k.to_lowercase();
                if key == "content-length" {
                    content_length = v.parse().unwrap_or(0);
                }
                headers.insert(key, v.to_string());
            }
        }

        // Body
        let mut body_bytes = vec![0u8; content_length];
        reader.read_exact(&mut body_bytes).await.unwrap();
        let body: Value = serde_json::from_slice(&body_bytes).unwrap_or(Value::Null);

        let _ = tx.send(CapturedRequest {
            method,
            path,
            headers,
            body,
        });

        // Write response — Content-Length so reqwest knows when to stop
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

// ── OpenAI-compat request body ────────────────────────────────────────────────

#[tokio::test]
async fn openai_compat_sends_correct_request_body() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hi"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        max_tokens: Some(32),
        temperature: Some(0.5),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::system("be brief"), Message::user("hello")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/v1/chat/completions");
    assert_eq!(req.body["model"], "gpt-4o-mini");
    // OpenAI now uses "max_completion_tokens" instead of "max_tokens"
    assert_eq!(req.body["max_completion_tokens"], 32);
    assert!((req.body["temperature"].as_f64().unwrap() - 0.5).abs() < 0.01);
    assert_eq!(req.body["stream"], true);
    let msgs = req.body["messages"].as_array().expect("messages array");
    assert_eq!(msgs.len(), 2, "system + user");
    assert_eq!(msgs[0]["role"], "system");
    assert_eq!(msgs[1]["role"], "user");
}

#[tokio::test]
async fn openai_compat_sends_bearer_auth_header() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-bearer-token".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let auth = req
        .headers
        .get("authorization")
        .expect("Authorization header");
    assert_eq!(auth, "Bearer sk-bearer-token");
}

#[tokio::test]
async fn openai_compat_formats_tools_correctly() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let tool = ToolSchema {
        name: "shell".into(),
        description: "run shell commands".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "cmd": { "type": "string" } },
            "required": ["cmd"],
        }),
    };
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("run ls")],
            tools: vec![tool],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let tools = req.body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "shell");
    assert_eq!(tools[0]["function"]["description"], "run shell commands");
    assert!(tools[0]["function"]["parameters"].is_object());
    // OpenAI uses "parameters" (not "input_schema")
    assert!(tools[0]["function"].get("input_schema").is_none());
}

// ── OpenAI-compat SSE event parsing ──────────────────────────────────────────

#[tokio::test]
async fn openai_compat_text_and_usage_events_collected() {
    let sse = sse_body(&[
        r#"{"choices":[{"delta":{"content":"hel"}}]}"#,
        r#"{"choices":[{"delta":{"content":"lo"}}]}"#,
        r#"{"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
    ]);
    let (port, _) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("say hello")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut text = String::new();
    let mut usage_seen = false;
    let mut done_seen = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ResponseEvent::TextDelta(t) if !t.is_empty() => text.push_str(&t),
            ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..
            } => usage_seen = true,
            ResponseEvent::Done => {
                done_seen = true;
                break;
            }
            _ => {}
        }
    }

    assert_eq!(text, "hello", "text deltas must concatenate to 'hello'");
    assert!(usage_seen, "Usage(10,5) event must be emitted");
    assert!(done_seen, "Done event must be emitted after [DONE]");
}

#[tokio::test]
async fn openai_compat_tool_call_events_collected() {
    let sse = sse_body(&[
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_1","function":{"name":"shell","arguments":""}}]}}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"id":"","function":{"name":"","arguments":"{\"cmd\":\"ls\"}"}}]}}]}"#,
    ]);
    let (port, _) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("run ls")],
            tools: vec![ToolSchema {
                name: "shell".into(),
                description: "runs shell commands".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut tool_events: Vec<ResponseEvent> = vec![];
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            e @ ResponseEvent::ToolCall { .. } => tool_events.push(e),
            ResponseEvent::Done => break,
            _ => {}
        }
    }

    assert!(!tool_events.is_empty(), "expected ToolCall events");
    assert!(
        matches!(&tool_events[0], ResponseEvent::ToolCall { id, name, .. }
            if id == "call_1" && name == "shell"),
        "first event should be ToolCall with id=call_1 name=shell"
    );
}

// ── OpenAI-compat error response ──────────────────────────────────────────────

#[tokio::test]
async fn openai_compat_non_200_response_returns_error() {
    let (port, _) = mock_server_once(
        401,
        "application/json",
        r#"{"error":{"message":"Unauthorized","type":"invalid_request_error"}}"#,
    )
    .await;

    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("bad-key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let result = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await;

    assert!(result.is_err(), "non-200 response must produce an error");
    let msg = result.err().unwrap().to_string();
    assert!(
        msg.contains("401"),
        "error message should include status 401, got: {msg}"
    );
}

// ── Azure OpenAI driver ───────────────────────────────────────────────────────

#[tokio::test]
async fn azure_sends_api_key_header_not_bearer() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"hello"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    // Azure: specify base_url (the path up to but not including
    // `/chat/completions`) — the driver appends `?api-version=…` itself.
    let cfg = ModelConfig {
        provider: "azure".into(),
        name: "gpt-4o".into(),
        api_key: Some("azure-secret-key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/openai/deployments/gpt-4o")),
        azure_api_version: Some("2024-02-01".into()),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    // Azure uses `api-key` header, NOT `Authorization: Bearer …`
    assert_eq!(
        req.headers.get("api-key").expect("api-key header"),
        "azure-secret-key"
    );
    assert!(
        req.headers.get("authorization").is_none(),
        "Azure must not send an Authorization header"
    );
    // URL must include the api-version query parameter
    assert!(
        req.path.contains("api-version=2024-02-01"),
        "path should include api-version, got: {}",
        req.path
    );
}

// ── Anthropic driver ──────────────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_sends_correct_request_format() {
    let sse = concat!(
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
        "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"hi\"}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    // Disable caching so the system block is a plain string — this test
    // focuses on request routing and auth headers, not caching behaviour.
    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("sk-ant-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        max_tokens: Some(64),
        cache_system_prompt: false,
        cache_tools: false,
        cache_conversation: false,
        cache_images: false,
        cache_tool_results: false,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::system("be brief"), Message::user("hello")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();

    let mut got_text = false;
    let mut got_done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ResponseEvent::TextDelta(t) if t == "hi" => got_text = true,
            ResponseEvent::Done => {
                got_done = true;
                break;
            }
            _ => {}
        }
    }
    assert!(got_text, "expected TextDelta('hi')");
    assert!(got_done, "expected Done");

    let req = req_rx.await.unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/v1/messages");

    // Auth header: x-api-key, not Bearer
    assert_eq!(
        req.headers.get("x-api-key").expect("x-api-key header"),
        "sk-ant-test"
    );
    assert!(
        req.headers.get("authorization").is_none(),
        "Anthropic must not send Authorization header"
    );

    // Version header
    assert_eq!(
        req.headers
            .get("anthropic-version")
            .expect("anthropic-version"),
        "2023-06-01"
    );

    // System prompt extracted to top-level field (plain string when caching off)
    assert_eq!(req.body["system"], "be brief");

    // Messages must not include the system message
    let msgs = req.body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[tokio::test]
async fn anthropic_tools_use_input_schema_not_parameters() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![ToolSchema {
                name: "shell".into(),
                description: "run commands".into(),
                parameters: serde_json::json!({"type":"object","properties":{"cmd":{"type":"string"}}}),
            }],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let tools = req.body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "shell");
    // Anthropic requires "input_schema", not "parameters"
    assert!(
        tools[0]["input_schema"].is_object(),
        "must use 'input_schema'"
    );
    assert!(
        tools[0].get("parameters").is_none(),
        "must not use 'parameters'"
    );
}

#[tokio::test]
async fn anthropic_cache_tools_adds_cache_control_to_last_tool() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_tools: true,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![
                ToolSchema {
                    name: "read_file".into(),
                    description: "read a file".into(),
                    parameters: serde_json::json!({"type":"object"}),
                },
                ToolSchema {
                    name: "shell".into(),
                    description: "run commands".into(),
                    parameters: serde_json::json!({"type":"object"}),
                },
            ],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let tools = req.body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 2);
    // Only the LAST tool gets cache_control.
    assert!(
        tools[0].get("cache_control").is_none(),
        "first tool must NOT have cache_control"
    );
    assert_eq!(
        tools[1]["cache_control"]["type"], "ephemeral",
        "last tool must have cache_control ephemeral"
    );
    // 5m TTL: no ttl field
    assert!(
        tools[1]["cache_control"].get("ttl").is_none(),
        "5m TTL should have no ttl field"
    );
    // beta header must be present
    let beta = req
        .headers
        .get("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(
        beta.contains("prompt-caching-2024-07-31"),
        "beta header must include prompt-caching"
    );
}

#[tokio::test]
async fn anthropic_cache_tools_with_extended_ttl_adds_1h_cache_control() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_tools: true,
        extended_cache_time: true,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![ToolSchema {
                name: "shell".into(),
                description: "run commands".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let tools = req.body["tools"].as_array().expect("tools array");
    assert_eq!(tools[0]["cache_control"]["ttl"], "1h", "1h TTL must be set");
    // Beta header must include both caching and extended-ttl entries
    let beta = req
        .headers
        .get("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(beta.contains("prompt-caching-2024-07-31"));
    assert!(beta.contains("extended-cache-ttl-2025-04-11"));
}

#[tokio::test]
async fn anthropic_cache_conversation_adds_top_level_cache_control() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_conversation: true,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("first message"),
                Message::assistant("first response"),
                Message::user("follow-up question"),
            ],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    // Automatic caching: top-level cache_control must be present
    assert_eq!(
        req.body["cache_control"]["type"], "ephemeral",
        "top-level cache_control must be set for conversation caching"
    );
    let beta = req
        .headers
        .get("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(beta.contains("prompt-caching-2024-07-31"));
}

#[tokio::test]
async fn anthropic_no_caching_sends_no_beta_header() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        // Explicitly disable every caching flag — defaults are now all true.
        cache_system_prompt: false,
        cache_tools: false,
        cache_conversation: false,
        cache_images: false,
        cache_tool_results: false,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hi")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    assert!(
        req.headers.get("anthropic-beta").is_none(),
        "no beta header should be sent when caching is disabled"
    );
}

#[tokio::test]
async fn anthropic_cache_system_prompt_sends_array_with_cache_control() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_system_prompt: true,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::system("be helpful"), Message::user("hi")],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    // System must be an array of blocks, not a plain string
    let system = req.body["system"]
        .as_array()
        .expect("system must be an array when caching");
    assert_eq!(system.len(), 1, "one system block (no dynamic suffix)");
    assert_eq!(system[0]["type"], "text");
    assert_eq!(system[0]["text"], "be helpful");
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
}

// ── Per-block history caching ─────────────────────────────────────────────────

#[tokio::test]
async fn anthropic_cache_images_marks_image_block_with_cache_control() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_images: true,
        ..ModelConfig::default()
    };

    let data_url = "data:image/png;base64,iVBORw0KGgo=";
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user_with_parts(vec![
                ContentPart::image(data_url),
                ContentPart::Text {
                    text: "what is in this image?".into(),
                },
            ])],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();
    let content = msgs[0]["content"].as_array().unwrap();

    // Image block must have cache_control; text block must not.
    let img = &content[0];
    assert_eq!(img["type"], "image");
    assert_eq!(
        img["cache_control"]["type"], "ephemeral",
        "image block must be marked with cache_control"
    );
    assert!(
        img["cache_control"].get("ttl").is_none(),
        "default TTL: no ttl field"
    );

    let txt = &content[1];
    assert!(
        txt.get("cache_control").is_none(),
        "text block must NOT get cache_control"
    );

    // Beta header required for caching
    let beta = req
        .headers
        .get("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(beta.contains("prompt-caching-2024-07-31"));
}

#[tokio::test]
async fn anthropic_cache_images_with_extended_ttl() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_images: true,
        extended_cache_time: true,
        ..ModelConfig::default()
    };

    let data_url = "data:image/jpeg;base64,/9j/4AAQ=";
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user_with_parts(vec![ContentPart::image(data_url)])],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();
    let img = &msgs[0]["content"][0];
    assert_eq!(img["cache_control"]["type"], "ephemeral");
    assert_eq!(img["cache_control"]["ttl"], "1h", "extended TTL must be 1h");

    let beta = req
        .headers
        .get("anthropic-beta")
        .expect("anthropic-beta header");
    assert!(beta.contains("extended-cache-ttl-2025-04-11"));
}

#[tokio::test]
async fn anthropic_cache_tool_results_marks_large_result() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_tool_results: true,
        ..ModelConfig::default()
    };

    // Build a tool result that exceeds the 4096-char threshold.
    let large_content = "x".repeat(5000);

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("run ls"),
                Message::tool_result("call_abc", &large_content),
            ],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();

    // Second message is the tool result.
    let block = &msgs[1]["content"][0];
    assert_eq!(block["type"], "tool_result");
    assert_eq!(
        block["cache_control"]["type"], "ephemeral",
        "large tool result must be marked with cache_control"
    );
}

#[tokio::test]
async fn anthropic_cache_tool_results_skips_small_result() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_tool_results: true,
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("run ls"),
                Message::tool_result("call_abc", "file.txt"), // tiny result
            ],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();
    let block = &msgs[1]["content"][0];
    assert!(
        block.get("cache_control").is_none(),
        "small tool result must NOT be cached"
    );
}

#[tokio::test]
async fn anthropic_cache_respects_4_breakpoint_budget() {
    // With system (1) + tools (1) + conversation (1) = 3 slots used,
    // only 1 remaining slot is available.  With two images in the conversation,
    // only the FIRST (oldest) image should be marked.
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        cache_system_prompt: true,
        cache_tools: true,
        cache_conversation: true,
        cache_images: true,
        ..ModelConfig::default()
    };

    let data_url = "data:image/png;base64,iVBORw0KGgo=";
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![
                Message::system("be helpful"),
                Message::user_with_parts(vec![ContentPart::image(data_url)]),
                Message::assistant("first"),
                Message::user_with_parts(vec![ContentPart::image(data_url)]),
            ],
            tools: vec![ToolSchema {
                name: "shell".into(),
                description: "run".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();

    // msgs[0] is the first user message (system was extracted); its image gets slot 4.
    let first_img = &msgs[0]["content"][0];
    assert_eq!(first_img["type"], "image");
    assert_eq!(
        first_img["cache_control"]["type"], "ephemeral",
        "oldest image must be cached (uses the 4th slot)"
    );

    // msgs[2] is the second user message; its image should NOT be cached (budget exhausted).
    let second_img = &msgs[2]["content"][0];
    assert_eq!(second_img["type"], "image");
    assert!(
        second_img.get("cache_control").is_none(),
        "second image must NOT be cached when budget is exhausted"
    );
}

#[tokio::test]
async fn anthropic_cache_images_disabled_leaves_no_cache_control() {
    // Verify that a single flag can be turned off without affecting the image
    // block — even though other caching remains active (the default).
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        // Explicitly opt out of image caching while keeping the rest active.
        cache_images: false,
        ..ModelConfig::default()
    };

    let data_url = "data:image/png;base64,iVBORw0KGgo=";
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user_with_parts(vec![ContentPart::image(data_url)])],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();
    let img = &msgs[0]["content"][0];
    assert!(
        img.get("cache_control").is_none(),
        "image block must NOT have cache_control when cache_images is false"
    );
    // Other caching (system, conversation) is still active so the beta header
    // IS present — only the image block itself must be unmarked.
    assert!(
        req.headers.get("anthropic-beta").is_some(),
        "beta header must still be sent for other active caching layers"
    );
}

#[tokio::test]
async fn anthropic_tool_result_message_mapped_to_user_role() {
    let sse = "data: {\"type\":\"message_stop\"}\n\n";
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("key".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![
                Message::user("run ls"),
                // Simulated tool result turn
                Message::tool_result("call_123", "file.txt\nother.txt"),
            ],
            tools: vec![],
            stream: true,
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let msgs = req.body["messages"].as_array().unwrap();
    // Both messages should be in the messages array (system prompt is absent)
    assert_eq!(msgs.len(), 2);
    // Tool result must be sent as role=user with a tool_result content block
    assert_eq!(msgs[1]["role"], "user");
    let content = &msgs[1]["content"][0];
    assert_eq!(content["type"], "tool_result");
    assert_eq!(content["tool_use_id"], "call_123");
}

// ── OpenRouter prompt_cache_key ───────────────────────────────────────────────

/// OpenRouter requests must include `prompt_cache_key` in the body when the
/// `CompletionRequest.cache_key` is set, so all turns in a session share the
/// same cached prefix at the gateway level.
#[tokio::test]
async fn openrouter_sends_prompt_cache_key_when_set() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "openrouter".into(),
        name: "anthropic/claude-3-haiku".into(),
        api_key: Some("sk-or-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/api/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hello")],
            stream: true,
            cache_key: Some("test-session-uuid-1234".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    assert_eq!(
        req.body["prompt_cache_key"], "test-session-uuid-1234",
        "OpenRouter requests must carry the session cache key"
    );
}

/// Non-OpenRouter providers (e.g. groq) must NOT have `prompt_cache_key`
/// injected even when `cache_key` is set.
#[tokio::test]
async fn non_openrouter_provider_does_not_send_prompt_cache_key() {
    let sse = sse_body(&[r#"{"choices":[{"delta":{"content":"ok"}}]}"#]);
    let (port, req_rx) = mock_server_once(200, "text/event-stream", sse).await;

    let cfg = ModelConfig {
        provider: "groq".into(),
        name: "llama-3.3-70b-versatile".into(),
        api_key: Some("gsk_test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/openai/v1")),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::user("hello")],
            stream: true,
            cache_key: Some("test-session-uuid-1234".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    assert!(
        req.body.get("prompt_cache_key").is_none() || req.body["prompt_cache_key"].is_null(),
        "Non-OpenRouter providers must not receive prompt_cache_key in the body"
    );
}
