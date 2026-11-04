// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents on locally served reasoning models
// for its clients. If your team needs expertise in running thinking models in
// agent loops you can procure our services by sending an email to
// info@swedishembedded.com.
//! Spec: the `brain` provider sends `chat_template_kwargs.enable_thinking`
//! when the configuration says so, and nothing when it does not.

use futures::StreamExt;
use serde_json::{json, Value};
use sven_model::{CompletionRequest, Message};
use sven_model_drivers::{from_config, ModelConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Serves one chat completion and returns the request body it received.
async fn body_sent_to_brain(configure: impl FnOnce(&mut ModelConfig)) -> Value {
    body_sent_with(configure, None).await
}

/// As [`body_sent_to_brain`], for a request that sets the thinking switch itself.
async fn body_sent_with(
    configure: impl FnOnce(&mut ModelConfig),
    enable_thinking: Option<bool>,
) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<Value>();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
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
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
            sse.len()
        );
        let _ = write_half.write_all(reply.as_bytes()).await;
    });
    let mut cfg = ModelConfig {
        provider: "brain".into(),
        name: "m".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    configure(&mut cfg);
    let provider = from_config(&cfg).unwrap();
    let request = CompletionRequest {
        messages: vec![Message::user("hello")],
        stream: true,
        enable_thinking,
        ..Default::default()
    };
    let mut stream = provider.complete(request).await.unwrap();
    while stream.next().await.is_some() {}
    rx.await.unwrap()
}

#[tokio::test]
async fn a_configured_enable_thinking_is_sent_as_a_template_argument() {
    let body = body_sent_to_brain(|cfg| cfg.enable_thinking = Some(false)).await;
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false)
    );
    let body = body_sent_to_brain(|cfg| cfg.enable_thinking = Some(true)).await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
}

#[tokio::test]
async fn nothing_is_sent_when_the_configuration_is_silent() {
    let body = body_sent_to_brain(|_| {}).await;
    assert!(body.get("chat_template_kwargs").is_none(), "{body}");
}

#[tokio::test]
async fn an_explicit_driver_option_wins() {
    let body = body_sent_to_brain(|cfg| {
        cfg.enable_thinking = Some(false);
        cfg.driver_options = json!({"chat_template_kwargs": {"enable_thinking": true, "other": 1}});
    })
    .await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
    assert_eq!(body["chat_template_kwargs"]["other"], json!(1));
}

#[tokio::test]
async fn a_request_that_sets_the_switch_beats_the_configuration_and_the_driver_options() {
    let body = body_sent_with(|cfg| cfg.enable_thinking = Some(true), Some(false)).await;
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false)
    );
    let body = body_sent_with(
        |cfg| {
            cfg.driver_options =
                json!({"chat_template_kwargs": {"enable_thinking": true, "other": 1}})
        },
        Some(false),
    )
    .await;
    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"],
        json!(false)
    );
    assert_eq!(
        body["chat_template_kwargs"]["other"],
        json!(1),
        "the rest of the arguments stay"
    );
    let body = body_sent_with(|_| {}, Some(true)).await;
    assert_eq!(body["chat_template_kwargs"]["enable_thinking"], json!(true));
}
