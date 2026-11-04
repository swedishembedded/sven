// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting agents to the
// services their people use, reliably, for its clients. If your team needs
// expertise in MCP client integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! A Streamable HTTP server slow to answer its first requests (one still
//! starting, a route compiled on its first request) is still reached: the
//! client's discovery gives up on it, and the late answer to discovery must
//! not be taken for the answer to `initialize`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sven_mcp_client::{McpManager, McpServerConfig, McpTransport, TokenStore};
use tokio::sync::mpsc;

/// A Streamable HTTP server of the 2025-11-25 protocol that holds every
/// request until `hold` after the first arrived (as a server still starting
/// does, or a route compiled on its first request), then answers the newer
/// protocol's `server/discover` as unknown first and `initialize` just
/// after: past the client's discovery timeout, so the late answer to
/// discovery arrives while the client waits for the one to `initialize`.
async fn serve_slow_legacy_http(hold: Duration) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let first = Arc::new(std::sync::OnceLock::<tokio::time::Instant>::new());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let first = Arc::clone(&first);
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let end = loop {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let length: usize = head
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.trim() == "content-length")
                    .and_then(|(_, v)| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < end + length {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let release = *first.get_or_init(tokio::time::Instant::now) + hold;
                let request: serde_json::Value =
                    serde_json::from_slice(&buf[end..]).unwrap_or_default();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let outcome = match method.as_str() {
                    "server/discover" => Some((
                        "error",
                        json!({ "code": -32601, "message": "method not found" }),
                    )),
                    "initialize" => Some((
                        "result",
                        json!({
                            "protocolVersion": "2025-11-25",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "slow", "version": "1" },
                        }),
                    )),
                    "tools/list" => Some((
                        "result",
                        json!({ "tools": [{
                        "name": "ping", "description": "Answers pong.", "inputSchema": { "type": "object" },
                    }] }),
                    )),
                    _ => None,
                };
                let at = if method == "initialize" {
                    release + Duration::from_millis(300)
                } else {
                    release
                };
                tokio::time::sleep_until(at).await;
                let reply = match (head.starts_with("post "), request.get("id"), outcome) {
                    (true, Some(id), Some((key, value))) => {
                        let mut body = json!({ "jsonrpc": "2.0", "id": id });
                        body[key] = value;
                        let body = body.to_string();
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    (true, _, _) => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                    _ => "HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into(),
                };
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamable_http_server_slow_to_answer_its_first_requests_is_still_reached() {
    let url = serve_slow_legacy_http(Duration::from_secs(11)).await;
    let dir = tempfile::tempdir().unwrap();
    let (tx, _events) = mpsc::channel(64);
    let config = McpServerConfig {
        transport: McpTransport::Http {
            url,
            headers: HashMap::new(),
        },
        timeout_secs: 30,
        ..McpServerConfig::default()
    };
    let manager = McpManager::with_token_store(
        [("slow".to_string(), config)].into_iter().collect(),
        tx,
        false,
        TokenStore::new(dir.path().join("mcp-oauth.json")),
    );
    manager.connect("slow").await.unwrap();
    let names: Vec<String> = manager
        .tools()
        .await
        .iter()
        .map(|t| sven_tool_api::Tool::name(t).to_string())
        .collect();
    assert_eq!(names, ["slow-ping"]);
}
