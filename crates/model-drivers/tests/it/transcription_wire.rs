// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: `transcribe` posts one multipart request to `{base}/audio/transcriptions`
//! with the audio as `file`, the `model` and, when given, the `language`, under
//! the caller's bearer key; it returns the `text` of the answer, and names the
//! server's own `error.message` when the status is not a success.
//!
//! Swedish Embedded AB implements solutions for speech input to agent
//! interfaces for its clients. If your team needs expertise in streaming
//! speech recognition then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde_json::{json, Value};
use sven_model_drivers::transcribe;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// What the mock server saw: request line, headers (lower-cased names) and body.
struct Seen {
    path: String,
    authorization: Option<String>,
    content_type: String,
    body: Vec<u8>,
}

async fn serve(status: u16, answer: Value) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Seen>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
        let (mut length, mut authorization, mut content_type) = (0usize, None, String::new());
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).await.unwrap();
            let header = header.trim();
            if header.is_empty() {
                break;
            }
            if let Some((k, v)) = header.split_once(": ") {
                match k.to_ascii_lowercase().as_str() {
                    "content-length" => length = v.parse().unwrap_or(0),
                    "authorization" => authorization = Some(v.to_owned()),
                    "content-type" => content_type = v.to_owned(),
                    _ => {}
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).await.unwrap();
        let _ = tx.send(Seen {
            path,
            authorization,
            content_type,
            body,
        });
        let body = answer.to_string();
        let reply = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = write.write_all(reply.as_bytes()).await;
    });
    (port, rx)
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn posts_file_model_and_language_and_returns_the_text() {
    let (port, mut seen) = serve(
        200,
        json!({"text": "hello world", "usage": {"seconds": 1.5}}),
    )
    .await;
    let text = transcribe(
        &format!("http://127.0.0.1:{port}/v1/"),
        "secret-key",
        "asr-model",
        Some("sv"),
        b"OggS-bytes".to_vec(),
    )
    .await
    .expect("a transcript");
    assert_eq!(text, "hello world");

    let request = seen.recv().await.unwrap();
    assert_eq!(request.path, "/v1/audio/transcriptions");
    assert_eq!(request.authorization.as_deref(), Some("Bearer secret-key"));
    assert!(request.content_type.starts_with("multipart/form-data"));
    assert!(contains(&request.body, "name=\"file\""));
    assert!(contains(&request.body, "audio/ogg"));
    assert!(contains(&request.body, "OggS-bytes"));
    assert!(contains(&request.body, "name=\"model\""));
    assert!(contains(&request.body, "asr-model"));
    assert!(contains(&request.body, "name=\"language\""));
}

#[tokio::test]
async fn language_is_omitted_when_not_given() {
    let (port, mut seen) = serve(200, json!({"text": "x"})).await;
    transcribe(
        &format!("http://127.0.0.1:{port}/v1"),
        "k",
        "m",
        None,
        vec![1],
    )
    .await
    .unwrap();
    assert!(!contains(
        &seen.recv().await.unwrap().body,
        "name=\"language\""
    ));
}

#[tokio::test]
async fn a_refusal_names_the_status_and_the_servers_message() {
    let (port, _seen) = serve(402, json!({"error": {"message": "balance exhausted"}})).await;
    let err = transcribe(
        &format!("http://127.0.0.1:{port}/v1"),
        "k",
        "m",
        None,
        vec![1],
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(err.contains("402"), "{err}");
    assert!(err.contains("balance exhausted"), "{err}");
}

#[tokio::test]
async fn an_answer_without_text_is_an_error() {
    let (port, _seen) = serve(200, json!({"usage": {"seconds": 1.0}})).await;
    let err = transcribe(
        &format!("http://127.0.0.1:{port}/v1"),
        "k",
        "m",
        None,
        vec![1],
    )
    .await;
    assert!(err.is_err());
}
