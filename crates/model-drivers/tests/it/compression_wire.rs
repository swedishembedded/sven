// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that stay usable over a slow link for
// its clients. If your team needs expertise in bandwidth-lean LLM clients
// then you can procure our services by sending an email to
// info@swedishembedded.com.
//! A request to the sven service goes compressed once the service has said it
//! takes compressed bodies (`Accept-Encoding` on its answers, RFC 7694): the
//! whole conversation is sent with every step, and JSON compresses several
//! times over. Until then, and to an older service that never says so, it
//! goes plainly. A server that refuses an encoded body (415) is asked again
//! plainly, and from then on plainly; any other provider is always asked
//! plainly.

use std::io::Read as _;

use futures::StreamExt;
use serde_json::Value;
use sven_model::{CompletionRequest, Message};
use sven_model_drivers::{from_config, ModelConfig};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What a request arrived as: its `Content-Encoding`, the bytes on the wire
/// and the JSON body they decode to.
#[derive(Debug)]
struct Received {
    encoding: Option<String>,
    wire_bytes: usize,
    body: Value,
}

/// Serves `n` requests on a loopback port; with `advertise`, every answer
/// says `Accept-Encoding: gzip`. With `refuse_encoded`, a body sent encoded
/// is answered 415, as a server that takes none does; anything else gets a
/// short streamed reply.
async fn serve(
    n: usize,
    advertise: bool,
    refuse_encoded: bool,
) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Received>) {
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
            let (mut length, mut encoding) = (0usize, None);
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
                    if k.eq_ignore_ascii_case("content-encoding") {
                        encoding = Some(v.to_ascii_lowercase());
                    }
                }
            }
            let mut wire = vec![0u8; length];
            reader.read_exact(&mut wire).await.unwrap();
            let plain = match encoding.as_deref() {
                Some("gzip") => {
                    let mut out = Vec::new();
                    flate2::read::GzDecoder::new(wire.as_slice())
                        .read_to_end(&mut out)
                        .unwrap();
                    out
                }
                _ => wire.clone(),
            };
            let refused = refuse_encoded && encoding.is_some();
            let _ = tx.send(Received {
                encoding,
                wire_bytes: wire.len(),
                body: serde_json::from_slice(&plain).unwrap_or(Value::Null),
            });
            let offer = if advertise {
                "Accept-Encoding: gzip\r\n"
            } else {
                ""
            };
            let (status, kind, body) = if refused {
                (
                    "415 Unsupported Media Type",
                    "application/json",
                    r#"{"error":{"message":"send the body unencoded"}}"#,
                )
            } else {
                (
                    "200 OK",
                    "text/event-stream",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n",
                )
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\n{offer}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = write.write_all(reply.as_bytes()).await;
        }
    });
    (port, rx)
}

fn config(provider: &str, port: u16) -> ModelConfig {
    ModelConfig {
        provider: provider.into(),
        name: "some-model".into(),
        base_url: Some(format!("http://127.0.0.1:{port}/v1")),
        api_key: Some("k".into()),
        ..ModelConfig::default()
    }
}

/// A long conversation, as the steps of a task make one.
fn request() -> CompletionRequest {
    let history: Vec<Message> = (0..40)
        .flat_map(|i| {
            [
                Message::user(format!("step {i}: run the tests and report what failed")),
                Message::assistant(format!(
                    "ran them; test_{i} failed: assertion left == right"
                )),
            ]
        })
        .collect();
    CompletionRequest {
        messages: history,
        ..CompletionRequest::default()
    }
}

async fn ask(provider: &dyn sven_model::ModelProvider) {
    let mut stream = provider.complete(request()).await.expect("answered");
    while let Some(event) = stream.next().await {
        event.expect("streams");
    }
}

#[tokio::test]
async fn a_request_to_the_sven_service_goes_compressed_once_it_says_it_takes_them() {
    let (port, mut received) = serve(2, true, false).await;
    let provider = from_config(&config("sven", port)).unwrap();
    ask(provider.as_ref()).await;
    ask(provider.as_ref()).await;
    let first = received.recv().await.unwrap();
    let second = received.recv().await.unwrap();
    assert_eq!(first.encoding, None, "before it said so");
    assert_eq!(second.encoding.as_deref(), Some("gzip"));
    assert_eq!(second.body, first.body, "the same request, decoded");
    let plain = serde_json::to_vec(&second.body).unwrap().len();
    assert!(
        second.wire_bytes * 4 < plain,
        "{} on the wire for {plain}",
        second.wire_bytes
    );
}

#[tokio::test]
async fn a_service_that_never_says_it_takes_compressed_bodies_is_asked_plainly() {
    let (port, mut received) = serve(2, false, false).await;
    let provider = from_config(&config("sven", port)).unwrap();
    ask(provider.as_ref()).await;
    ask(provider.as_ref()).await;
    assert_eq!(received.recv().await.unwrap().encoding, None);
    assert_eq!(received.recv().await.unwrap().encoding, None);
}

#[tokio::test]
async fn a_server_that_refuses_an_encoded_body_is_asked_plainly_from_then_on() {
    let (port, mut received) = serve(4, true, true).await;
    let provider = from_config(&config("sven", port)).unwrap();
    for _ in 0..3 {
        ask(provider.as_ref()).await;
    }
    let first = received.recv().await.unwrap();
    let compressed = received.recv().await.unwrap();
    let again = received.recv().await.unwrap();
    let later = received.recv().await.unwrap();
    assert_eq!(first.encoding, None);
    assert_eq!(compressed.encoding.as_deref(), Some("gzip"));
    assert_eq!(again.encoding, None, "asked again plainly");
    assert_eq!(again.body, compressed.body, "the same request");
    assert_eq!(later.encoding, None, "and plainly from then on");
}

#[tokio::test]
async fn any_other_provider_is_asked_plainly() {
    let (port, mut received) = serve(2, true, false).await;
    let provider = from_config(&config("openai", port)).unwrap();
    ask(provider.as_ref()).await;
    ask(provider.as_ref()).await;
    assert_eq!(received.recv().await.unwrap().encoding, None);
    assert_eq!(received.recv().await.unwrap().encoding, None);
}
