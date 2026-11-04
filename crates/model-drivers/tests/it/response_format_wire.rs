// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for schema-constrained agent steps
// against local model servers for its clients. If your team needs expertise in
// structured output from local models then you can procure our services by
// sending an email to info@swedishembedded.com.
//! What the OpenAI-compatible driver sends for a structured-output request:
//! `response_format` goes to servers that accept it, and is left out for the
//! `brain` server, which refuses the field (callers post-parse and retry, so
//! the constraint is an optimisation, not the contract).

use futures::StreamExt;
use serde_json::{json, Value};
use sven_model::{CompletionRequest, Message, ResponseFormat};
use sven_model_drivers::{from_config, ModelConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Serves one request on a loopback port and returns the body it received.
async fn serve_once() -> (u16, tokio::sync::oneshot::Receiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let mut length = 0usize;
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line.trim().is_empty() {
                break;
            }
            if let Some((k, v)) = line.trim().split_once(": ") {
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await.unwrap();
        let _ = tx.send(serde_json::from_slice(&body).unwrap_or(Value::Null));
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"{}\"}}]}\n\ndata: [DONE]\n\n";
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = write.write_all(reply.as_bytes()).await;
    });
    (port, rx)
}

async fn body_sent_by(provider: &str) -> Value {
    let (port, rx) = serve_once().await;
    let config = ModelConfig {
        provider: provider.into(),
        name: "some-model".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let model = from_config(&config).unwrap();
    let mut stream = model
        .complete(CompletionRequest {
            messages: vec![Message::user("classify")],
            stream: true,
            response_format: Some(ResponseFormat::JsonSchema {
                name: "route".into(),
                schema: json!({"type": "object"}),
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    while stream.next().await.is_some() {}
    rx.await.unwrap()
}

#[tokio::test]
async fn a_server_that_accepts_it_receives_the_json_schema() {
    let body = body_sent_by("openai").await;
    assert_eq!(body["response_format"]["type"], "json_schema", "{body}");
}

#[tokio::test]
async fn the_brain_server_is_not_sent_a_field_it_refuses() {
    let body = body_sent_by("brain").await;
    assert!(body.get("response_format").is_none(), "{body}");
}

/// Serves `n` requests on a loopback port: a body that carries
/// `response_format` is refused with the 400 a server without constrained
/// decoding answers; any other body gets a short streamed reply. Every body
/// received is sent back, in order.
async fn serve_refusing_response_format(
    n: usize,
) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for _ in 0..n {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let mut length = 0usize;
            loop {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                if line.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = line.trim().split_once(": ") {
                    if k.eq_ignore_ascii_case("content-length") {
                        length = v.parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let refused = body.get("response_format").is_some();
            let _ = tx.send(body);
            let reply = if refused {
                let error = r#"{"error":{"code":"invalid_request","message":"'response_format' is not supported","param":null,"type":"invalid_request_error"}}"#;
                format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{error}",
                    error.len()
                )
            } else {
                let sse =
                    "data: {\"choices\":[{\"delta\":{\"content\":\"{}\"}}]}\n\ndata: [DONE]\n\n";
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                    sse.len()
                )
            };
            let _ = write.write_all(reply.as_bytes()).await;
        }
    });
    (port, rx)
}

#[tokio::test]
async fn a_server_that_refuses_response_format_is_asked_again_without_it() {
    // A server addressed through the generic OpenAI wire format may have no
    // constrained decoding and refuse the field. The structured request then
    // goes through without it - the caller post-parses either way - and later
    // requests to that provider leave it out from the start.
    let (port, mut bodies) = serve_refusing_response_format(3).await;
    let config = ModelConfig {
        provider: "openai".into(),
        name: "some-model".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let model = from_config(&config).unwrap();
    for _ in 0..2 {
        let mut stream = model
            .complete(CompletionRequest {
                messages: vec![Message::user("classify")],
                stream: true,
                response_format: Some(ResponseFormat::JsonSchema {
                    name: "route".into(),
                    schema: json!({"type": "object"}),
                }),
                ..Default::default()
            })
            .await
            .expect("a refused response_format is retried without it");
        while stream.next().await.is_some() {}
    }
    let first = bodies.recv().await.unwrap();
    let retried = bodies.recv().await.unwrap();
    let later = bodies.recv().await.unwrap();
    assert!(first.get("response_format").is_some());
    assert!(retried.get("response_format").is_none());
    assert!(
        later.get("response_format").is_none(),
        "the refusal is remembered"
    );
}
