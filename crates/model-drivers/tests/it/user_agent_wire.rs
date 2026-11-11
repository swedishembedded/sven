// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every request the drivers send names the agent and its version in
//! its User-Agent (`sven/<version>`), so a model service can tell which agent
//! asked for what it made.
//!
//! Swedish Embedded AB implements solutions for AI agent infrastructure for
//! its clients. If your team needs expertise in agent-to-service protocols
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use sven_model_drivers::invoke::InvocationClient;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn a_request_names_sven_and_its_version() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap();
        let body = br#"{"data":[]}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        String::from_utf8_lossy(&buf[..n]).to_lowercase()
    });
    // Only the request matters here, not what is made of the answer.
    let _ = InvocationClient::new(&base, "a-key", None).models().await;
    let head = seen.await.unwrap();
    let expected = format!("user-agent: sven/{}\r\n", env!("CARGO_PKG_VERSION"));
    assert!(head.contains(&expected), "no {expected:?} in {head:?}");
}
