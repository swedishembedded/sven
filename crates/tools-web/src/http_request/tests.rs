// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn call(args: Value) -> ToolCall {
    ToolCall {
        id: "c1".into(),
        name: "http_request".into(),
        args,
    }
}

/// Echoes the request it received as the body of a 201.
async fn echo_server() -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut n = 0;
                // Read until the body (after the blank line) is in.
                loop {
                    let k = s.read(&mut buf[n..]).await.unwrap_or(0);
                    n += k;
                    let seen = String::from_utf8_lossy(&buf[..n]).to_string();
                    if k == 0
                        || seen.contains("\r\n\r\n")
                            && (seen.ends_with("ping") || !seen.starts_with("POST"))
                    {
                        break;
                    }
                }
                let echoed = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = format!(
                    "HTTP/1.1 201 Created\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{echoed}",
                    echoed.len()
                );
                let _ = s.write_all(reply.as_bytes()).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn a_post_carries_method_headers_and_body_and_returns_status_and_headers() {
    let addr = echo_server().await;
    let out = HttpRequestTool::default()
        .execute(&call(json!({
            "url": format!("http://{addr}/topic"),
            "method": "post",
            "headers": {"X-Title": "hello"},
            "body": "ping"
        })))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(
        out.content.starts_with("HTTP/1.1 201 Created"),
        "{}",
        out.content
    );
    assert!(out.content.contains("content-type: text/plain"));
    assert!(out.content.contains("POST /topic HTTP/1.1"));
    assert!(out.content.to_lowercase().contains("x-title: hello"));
    assert!(out.content.ends_with("ping"));
}

#[tokio::test]
async fn an_endless_answer_is_read_only_up_to_max_bytes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await;
        let _ = s
            .write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n")
            .await;
        let chunk = vec![b'z'; 16 * 1024];
        let mut sent = 0usize;
        // Far past anything the caller asked for: the reader must hang up.
        while s.write_all(&chunk).await.is_ok() && sent < 512 * 1024 * 1024 {
            sent += chunk.len();
        }
        let _ = done_tx.send(sent);
    });
    let out = HttpRequestTool::default()
        .execute(&call(
            json!({"url": format!("http://{addr}/"), "max_bytes": 1000}),
        ))
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("truncated"));
    assert!(out.content.matches('z').count() == 1000);
    let sent = tokio::time::timeout(Duration::from_secs(10), done_rx)
        .await
        .expect("the server saw the client hang up")
        .unwrap();
    assert!(sent < 512 * 1024 * 1024);
}

#[tokio::test]
async fn bad_arguments_are_refused_before_anything_is_sent() {
    let tool = HttpRequestTool::default();
    for (args, needle) in [
        (json!({}), "missing 'url'"),
        (json!({"url": "ftp://x/"}), "only http"),
        (
            json!({"url": "http://x/", "method": "TRACE"}),
            "unsupported method",
        ),
        (
            json!({"url": "http://x/", "headers": {"Host": "evil"}}),
            "transport",
        ),
        (
            json!({"url": "http://x/", "headers": {"a": 1}}),
            "must be a string",
        ),
        (
            json!({"url": "http://x/", "body": "a".repeat(MAX_REQUEST_BYTES + 1)}),
            "body larger",
        ),
    ] {
        let out = tool.execute(&call(args)).await;
        assert!(
            out.is_error && out.content.contains(needle),
            "{}",
            out.content
        );
    }
}

#[tokio::test]
async fn a_refused_status_is_an_answer_and_an_unreachable_host_an_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut b = [0u8; 1024];
        let _ = s.read(&mut b).await;
        let _ = s
            .write_all(
                b"HTTP/1.1 404 Not Found\r\ncontent-length: 4\r\nconnection: close\r\n\r\nnope",
            )
            .await;
    });
    let tool = HttpRequestTool::default();
    let out = tool
        .execute(&call(json!({"url": format!("http://{addr}/")})))
        .await;
    assert!(
        !out.is_error && out.content.starts_with("HTTP/1.1 404"),
        "{}",
        out.content
    );
    let out = tool
        .execute(&call(
            json!({"url": "http://127.0.0.1:1/", "timeout_secs": 2}),
        ))
        .await;
    assert!(out.is_error);
}

#[tokio::test]
async fn public_only_refuses_a_private_literal() {
    let out = HttpRequestTool::default()
        .public_only()
        .execute(&call(json!({"url": "http://127.0.0.1:9/"})))
        .await;
    assert!(
        out.is_error && out.content.contains("public"),
        "{}",
        out.content
    );
}
