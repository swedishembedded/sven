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
use sven_model::{from_config, CompletionRequest, Message, ResponseEvent, ToolSchema};
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
            if trimmed.is_empty() { break; }
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

        let _ = tx.send(CapturedRequest { method, path, headers, body });

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
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/v1/chat/completions");
    assert_eq!(req.body["model"], "gpt-4o-mini");
    assert_eq!(req.body["max_tokens"], 32);
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
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let auth = req.headers.get("authorization").expect("Authorization header");
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
        })
        .await
        .unwrap();

    let mut text = String::new();
    let mut usage_seen = false;
    let mut done_seen = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ResponseEvent::TextDelta(t) if !t.is_empty() => text.push_str(&t),
            ResponseEvent::Usage { input_tokens: 10, output_tokens: 5 } => usage_seen = true,
            ResponseEvent::Done => { done_seen = true; break; }
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
        })
        .await;

    assert!(result.is_err(), "non-200 response must produce an error");
    let msg = result.err().unwrap().to_string();
    assert!(msg.contains("401"), "error message should include status 401, got: {msg}");
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

    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-3-haiku-20240307".into(),
        api_key: Some("sk-ant-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}")),
        max_tokens: Some(64),
        ..ModelConfig::default()
    };

    let provider = from_config(&cfg).unwrap();
    let mut stream = provider
        .complete(CompletionRequest {
            messages: vec![Message::system("be brief"), Message::user("hello")],
            tools: vec![],
            stream: true,
        })
        .await
        .unwrap();

    let mut got_text = false;
    let mut got_done = false;
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            ResponseEvent::TextDelta(t) if t == "hi" => got_text = true,
            ResponseEvent::Done => { got_done = true; break; }
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
        req.headers.get("anthropic-version").expect("anthropic-version"),
        "2023-06-01"
    );

    // System prompt extracted to top-level field
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
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}

    let req = req_rx.await.unwrap();
    let tools = req.body["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "shell");
    // Anthropic requires "input_schema", not "parameters"
    assert!(tools[0]["input_schema"].is_object(), "must use 'input_schema'");
    assert!(tools[0].get("parameters").is_none(), "must not use 'parameters'");
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
