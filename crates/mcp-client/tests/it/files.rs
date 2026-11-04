// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a tool argument marked `x-mcp-file` takes a local path from the
//! model and reaches the server as the file it names, uploaded out of band
//! (`files/authorizeUpload`); the model's result never holds a transfer URL.
//! A file the server names is fetched (`files/authorizeDownload`) and kept
//! only when it matches its digest. A path outside the directory sven works
//! in is refused before anything is sent.
//!
//! Swedish Embedded AB implements solutions for giving AI agents safe access
//! to files across services for its clients. If your team needs expertise
//! in MCP integrations then you can procure our services by sending an email
//! to info@swedishembedded.com.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sven_mcp_client::{McpManager, McpServerConfig, McpTransport, TokenStore};
use sven_tool_api::{Tool, ToolCall};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Default)]
struct Seen {
    upload_params: Option<Value>,
    stored: Vec<u8>,
    lie_about_digest: bool,
}

fn digest(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

/// An MCP server over plain HTTP that also takes the uploads and serves the
/// downloads it grants.
async fn serve(seen: Arc<Mutex<Seen>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let url = format!("{base}/mcp");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let seen = Arc::clone(&seen);
            let base = base.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
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
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let lower = head.to_lowercase();
                let length: usize = lower
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
                let body = buf[end..].to_vec();
                let ok = |content_type: &str, body: &[u8]| {
                    let mut reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    reply.extend_from_slice(body);
                    reply
                };
                let reply = if lower.starts_with("put /upload/1") {
                    seen.lock().unwrap().stored = body;
                    ok("text/plain", b"")
                } else if lower.starts_with("get /download/1") {
                    let stored = seen.lock().unwrap().stored.clone();
                    ok("application/octet-stream", &stored)
                } else if lower.starts_with("post /mcp") {
                    let request: Value = serde_json::from_slice(&body).unwrap_or_default();
                    let method = request["method"].as_str().unwrap_or_default();
                    let result = match method {
                        "initialize" => Some(json!({
                            "protocolVersion": "2025-11-25",
                            "capabilities": { "tools": {} },
                            "serverInfo": { "name": "files", "version": "1" },
                        })),
                        "tools/list" => Some(json!({ "tools": [{
                            "name": "make_part",
                            "description": "Makes a part from a drawing.",
                            "inputSchema": { "type": "object", "properties": {
                                "drawing": { "type": "string", "format": "uri", "x-mcp-file": { "transferModes": ["upload"] } },
                                "title": { "type": "string" }
                            }},
                        }] })),
                        "files/authorizeUpload" => {
                            seen.lock().unwrap().upload_params = Some(request["params"].clone());
                            Some(json!({
                                "file": { "uri": "mcp-file://files/1", "name": request["params"]["name"] },
                                "upload": { "transport": "https", "method": "PUT", "url": format!("{base}/upload/1"),
                                            "headers": { "Content-Type": request["params"]["mimeType"] } },
                            }))
                        }
                        "files/authorizeDownload" => {
                            let state = seen.lock().unwrap();
                            let value = if state.lie_about_digest {
                                digest(b"other")
                            } else {
                                digest(&state.stored)
                            };
                            // As an MCP server SDK answers it: `resultType` and
                            // the whole file value beside the download.
                            Some(json!({
                                "resultType": "complete",
                                "file": { "uri": "mcp-file://files/1", "name": "../../part.step",
                                          "mimeType": "application/step", "size": state.stored.len(),
                                          "digest": { "algorithm": "sha-256", "value": value } },
                                "download": { "transport": "https", "method": "GET", "url": format!("{base}/download/1"),
                                              "expiresAt": "2026-11-04T20:00:00.000Z" },
                            }))
                        }
                        "tools/call" => Some(json!({ "content": [{
                            "type": "text", "text": format!("made from {} for {}", request["params"]["arguments"]["drawing"], request["params"]["_meta"]["sven/session"]),
                        }] })),
                        _ => None,
                    };
                    match (request.get("id"), result) {
                        (Some(id), Some(result)) => ok(
                            "application/json",
                            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string().as_bytes(),
                        ),
                        (Some(id), None) => ok(
                            "application/json",
                            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } })
                                .to_string()
                                .as_bytes(),
                        ),
                        _ => b"HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
                    }
                } else {
                    b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec()
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    url
}

/// Connected to the server at `url`, with file arguments resolved in `work`
/// (the directory the session works in) and the session named on requests.
async fn manager(url: String, dir: &std::path::Path, work: &std::path::Path) -> Arc<McpManager> {
    let (tx, _events) = mpsc::channel(64);
    let mut config = McpServerConfig {
        transport: McpTransport::Http {
            url,
            headers: HashMap::new(),
        },
        timeout_secs: 10,
        ..McpServerConfig::default()
    };
    config.file_root = Some(work.to_path_buf());
    config.request_meta.insert(
        "sven/session".into(),
        json!("6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10"),
    );
    let manager = McpManager::with_token_store(
        [("cad".to_string(), config)].into_iter().collect(),
        tx,
        false,
        TokenStore::new(dir.join("mcp-oauth.json")),
    );
    manager.connect("cad").await.unwrap();
    manager
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_argument_uploads_the_file_and_a_fetched_file_must_match_its_digest() {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let url = serve(Arc::clone(&seen)).await;
    // The session works in a directory with a drawing in it, not in the
    // process's current directory.
    let work = tempfile::tempdir().unwrap();
    std::fs::write(work.path().join("front.png"), b"\x89PNG drawing").unwrap();
    let home = tempfile::tempdir().unwrap();
    let manager = manager(url, home.path(), work.path()).await;

    let tools = manager.tools().await;
    let tool = tools
        .iter()
        .find(|t| t.name() == "cad-make_part")
        .expect("the tool is offered");
    let output = tool
        .execute(&ToolCall {
            id: "c1".into(),
            name: "cad-make_part".into(),
            args: json!({ "drawing": "front.png", "title": "bracket" }),
        })
        .await;
    assert!(!output.is_error, "{}", output.content);
    assert_eq!(
        output.content,
        "made from \"mcp-file://files/1\" for \"6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10\"",
        "the server got the file, not the path, and the call named its session"
    );
    assert!(
        !output.content.contains("/upload/"),
        "no transfer URL reaches the model"
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.stored, b"\x89PNG drawing",
            "the bytes went out of band"
        );
        let params = seen.upload_params.as_ref().unwrap();
        assert_eq!(params["name"], "front.png");
        assert_eq!(params["mimeType"], "image/png");
        assert_eq!(params["size"], 12);
        assert_eq!(params["digest"]["value"], digest(b"\x89PNG drawing"));
        assert_eq!(
            params["_meta"]["sven/session"],
            "6f1c1d1e-6c1a-4d43-9a43-6f2a9d3b2a10"
        );
    }

    // A path out of the directory sven works in is refused, and nothing is sent.
    seen.lock().unwrap().upload_params = None;
    let refused = tool
        .execute(&ToolCall {
            id: "c2".into(),
            name: "cad-make_part".into(),
            args: json!({ "drawing": "../../etc/passwd" }),
        })
        .await;
    assert!(refused.is_error);
    assert!(seen.lock().unwrap().upload_params.is_none());

    // Fetching it back: kept under a name that stays in the directory.
    let into = tempfile::tempdir().unwrap();
    let path = manager
        .download_file("cad", "mcp-file://files/1", into.path())
        .await
        .unwrap();
    assert_eq!(path, into.path().join("part.step"));
    assert_eq!(std::fs::read(&path).unwrap(), b"\x89PNG drawing");

    // One that does not match its digest is not kept.
    seen.lock().unwrap().lie_about_digest = true;
    let other = tempfile::tempdir().unwrap();
    let mismatch = manager
        .download_file("cad", "mcp-file://files/1", other.path())
        .await;
    assert!(mismatch.is_err());
    assert!(
        std::fs::read_dir(other.path()).unwrap().next().is_none(),
        "nothing was kept"
    );
}
