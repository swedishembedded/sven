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
