// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for agents that stay responsive
// when a tool server goes quiet for its clients. If your team needs expertise
// in connecting agents to Model Context Protocol servers then you can
// procure our services by sending an email to info@swedishembedded.com.
//! An MCP call must end at the server's `timeout_secs`, whichever way the
//! server goes quiet: it never answers, or it answers and stops mid-body.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use sven_mcp_client::protocol::JsonRpcRequest;
use sven_mcp_client::transport::{HttpTransport, StdioTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serves one connection: reads the request, sends headers announcing a long
/// body and a few bytes of it, then holds the socket open without a word.
async fn serve_headers_then_silence() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await;
        let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 4096\r\n\r\n{\"jsonrpc\"";
        socket.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    url
}

#[tokio::test]
async fn an_http_server_that_stalls_mid_body_fails_the_call_at_the_timeout() {
    let url = serve_headers_then_silence().await;
    let transport = HttpTransport::new(url, &HashMap::new(), 1, None).unwrap();
    let request = JsonRpcRequest::new(1, "tools/call", None);

    let started = Instant::now();
    let err = transport
        .send_request(&request)
        .await
        .expect_err("a stalled body must fail the call");

    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the call outlived its timeout: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_stdio_server_that_never_answers_fails_the_call_at_the_timeout() {
    let transport = StdioTransport::spawn("sleep", &["60".to_string()], &HashMap::new(), 1, None)
        .await
        .unwrap();
    let request = JsonRpcRequest::new(1, "tools/call", None);

    let started = Instant::now();
    let err = transport
        .send_request(&request)
        .await
        .expect_err("silence must fail the call");

    assert!(err.to_string().contains("timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
