// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for attributing the behaviour of
// LLM agents to the exact prompt that produced it for its clients. If your
// team needs expertise in reproducible prompt engineering then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Spec: the prompt manifest of a request is for the record and never goes
//! to a model. A provider's wire body carries the messages, tools and
//! settings of the request and nothing of the manifest, and a recorded
//! request keeps it.

use futures::StreamExt;
use sven_model::{CompletionRequest, Message, PromptManifest, PromptSections, ToolSchema};
use sven_model_drivers::from_config;
use sven_model_drivers::ModelConfig;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// A loopback server that answers one chat completion and hands back the raw
/// text of the request body it received.
async fn serve_once() -> (u16, tokio::sync::oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some((key, value)) = line.split_once(": ") {
                if key.eq_ignore_ascii_case("content-length") {
                    length = value.parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await.unwrap();
        let _ = tx.send(String::from_utf8(body).unwrap());
        let reply = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        let http = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        let _ = write_half.write_all(http.as_bytes()).await;
    });
    (port, rx)
}

fn request_with_manifest() -> CompletionRequest {
    let system = "You are a test.";
    let tool = ToolSchema {
        name: "probe".into(),
        description: "Probe.".into(),
        parameters: serde_json::json!({"type": "object"}),
        ..ToolSchema::default()
    };
    let messages = vec![Message::system(system), Message::user("hello")];
    let sections = PromptSections::new(
        system,
        &[("zz_marker_section".to_string(), 0..system.len())],
    );
    let manifest = PromptManifest::describe(
        &messages,
        Some(&sections),
        std::slice::from_ref(&tool),
        None,
    );
    CompletionRequest {
        messages,
        tools: vec![tool],
        stream: true,
        manifest,
        ..Default::default()
    }
}

#[tokio::test]
async fn the_openai_compatible_wire_body_carries_no_manifest() {
    let request = request_with_manifest();
    let manifest = request.manifest.clone().unwrap();
    let (port, body) = serve_once().await;
    let cfg = ModelConfig {
        provider: "openai".into(),
        name: "gpt-4o-mini".into(),
        api_key: Some("sk-test".into()),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        ..ModelConfig::default()
    };
    let provider = from_config(&cfg).unwrap();
    let mut stream = provider.complete(request).await.unwrap();
    while stream.next().await.is_some() {}

    let body = body.await.unwrap();
    assert!(body.contains("You are a test."), "the system text is sent");
    assert!(body.contains("probe"), "the tool is sent");
    for leaked in [
        "manifest",
        "zz_marker_section",
        "sections",
        "sha256",
        manifest.system_sha256.as_deref().unwrap(),
        manifest.tools_sha256.as_str(),
        manifest.tools[0].sha256.as_str(),
    ] {
        assert!(!body.contains(leaked), "the wire body carries {leaked:?}");
    }
}

#[test]
fn a_recorded_request_keeps_its_manifest() {
    let request = request_with_manifest();
    let line = serde_json::to_string(&request).unwrap();
    let back: CompletionRequest = serde_json::from_str(&line).unwrap();
    assert_eq!(back.manifest, request.manifest);

    // A log written before manifests existed still reads.
    let mut old: serde_json::Value = serde_json::from_str(&line).unwrap();
    old.as_object_mut().unwrap().remove("manifest");
    let back: CompletionRequest = serde_json::from_value(old).unwrap();
    assert!(back.manifest.is_none());
}
