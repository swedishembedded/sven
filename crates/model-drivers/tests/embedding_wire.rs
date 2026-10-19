// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the OpenAI-compatible embedder sends the texts of one call as one
//! `/v1/embeddings` request, returns one vector per text in the order asked
//! whatever order the server answers in, learns the vector length from the
//! first answer, and refuses an answer that cannot be used.
//!
//! Swedish Embedded AB implements solutions for retrieval over private agent
//! memory for its clients. If your team needs expertise in semantic search that
//! keeps tenants apart then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde_json::{json, Value};
use sven_memory_api::{Embedder, MemoryError};
use sven_model_drivers::OpenAiEmbedder;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// Serves each of `answers` to one request, in turn, and reports what was asked.
async fn serve(answers: Vec<(u16, Value)>) -> (u16, tokio::sync::mpsc::UnboundedReceiver<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        for (status, body) in answers {
            let (stream, _) = listener.accept().await.expect("accept");
            let (read, mut write) = stream.into_split();
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let mut length = 0usize;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).await.unwrap();
                let header = header.trim();
                if header.is_empty() {
                    break;
                }
                if let Some((k, v)) = header.split_once(": ") {
                    if k.eq_ignore_ascii_case("content-length") {
                        length = v.parse().unwrap_or(0);
                    }
                }
            }
            let mut raw = vec![0u8; length];
            reader.read_exact(&mut raw).await.unwrap();
            let _ = tx.send(serde_json::from_slice(&raw).unwrap_or(Value::Null));
            let body = body.to_string();
            let reply = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = write.write_all(reply.as_bytes()).await;
        }
    });
    (port, rx)
}

fn answer(vectors: &[(usize, [f32; 3])]) -> Value {
    json!({"object": "list", "model": "m", "data": vectors.iter()
        .map(|(index, v)| json!({"object": "embedding", "index": index, "embedding": v}))
        .collect::<Vec<_>>()})
}

#[tokio::test]
async fn one_request_per_call_vectors_in_the_order_asked_and_dims_learned() {
    let (port, mut asked) = serve(vec![
        (200, answer(&[(0, [1.0, 0.0, 0.0])])),
        // The server answers out of order.
        (200, answer(&[(1, [0.0, 1.0, 0.0]), (0, [1.0, 0.0, 0.0])])),
    ])
    .await;
    let embedder =
        OpenAiEmbedder::connect(format!("http://127.0.0.1:{port}/v1"), None, "embed-model")
            .await
            .expect("the probe answers");
    assert_eq!(embedder.spec().dims, 3);
    assert_eq!(embedder.spec().model_id, "embed-model");
    let vectors = embedder
        .embed(&["first".into(), "second".into()])
        .await
        .expect("an answer");
    assert_eq!(vectors, vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]]);
    let _probe = asked.recv().await.unwrap();
    let call = asked.recv().await.unwrap();
    assert_eq!(call["model"], "embed-model");
    assert_eq!(call["input"], json!(["first", "second"]));
}

#[tokio::test]
async fn an_unusable_answer_is_an_error_not_a_shorter_list() {
    let (port, _asked) = serve(vec![
        (200, answer(&[(0, [1.0, 0.0, 0.0])])),
        (200, answer(&[(0, [1.0, 0.0, 0.0])])),
        (500, json!({"error": {"message": "overloaded"}})),
    ])
    .await;
    let embedder = OpenAiEmbedder::connect(format!("http://127.0.0.1:{port}/v1"), None, "m")
        .await
        .unwrap();
    let short = embedder.embed(&["a".into(), "b".into()]).await;
    assert!(
        matches!(short, Err(MemoryError::Unavailable(_))),
        "{short:?}"
    );
    let failed = embedder.embed(&["a".into()]).await;
    assert!(
        matches!(failed, Err(MemoryError::Unavailable(_))),
        "{failed:?}"
    );
}
