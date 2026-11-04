// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the skills a server offers as `skill://` resources are written into
//! a directory of their own, as the files of skill directories; a resource
//! whose path climbs out is not written; a second sync replaces the first,
//! so a skill the server stopped offering is gone.
//!
//! Swedish Embedded AB implements solutions for distributing agent skills
//! from a service to every agent of its users for its clients. If your team
//! needs expertise in MCP integrations then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use sven_mcp_client::{McpManager, McpServerConfig, McpTransport, TokenStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

/// A server over plain HTTP whose resources are `offered` (uri to text).
async fn serve(offered: Arc<Mutex<Vec<(String, String)>>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let offered = Arc::clone(&offered);
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
                let request: Value = serde_json::from_slice(&buf[end..]).unwrap_or_default();
                let offered = offered.lock().unwrap().clone();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "initialize" => Some(json!({
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": {}, "resources": {} },
                        "serverInfo": { "name": "skills", "version": "1" },
                    })),
                    "tools/list" => Some(json!({ "tools": [] })),
                    "resources/list" => {
                        Some(json!({ "resources": offered.iter().map(|(uri, _)| json!({
                        "uri": uri, "name": uri, "mimeType": "text/markdown",
                    })).collect::<Vec<_>>() }))
                    }
                    "resources/read" => {
                        let uri = request["params"]["uri"].as_str().unwrap_or_default();
                        offered.iter().find(|(u, _)| u == uri).map(|(u, text)| json!({
                            "contents": [{ "uri": u, "mimeType": "text/markdown", "text": text }],
                        }))
                    }
                    _ => None,
                };
                let reply = match (request.get("id"), result) {
                    (Some(id), Some(result)) => {
                        let body =
                            json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
                        format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
                    }
                    _ => "HTTP/1.1 202 Accepted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string(),
                };
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread")]
async fn a_servers_skills_are_written_as_skill_directories_and_replaced_on_the_next_sync() {
    let offered = Arc::new(Mutex::new(vec![
        ("skill://case-manager/SKILL.md".to_string(), "---\nname: case-manager\ndescription: Interview for a case.\n---\nAsk what is missing.\n".to_string()),
        ("skill://case-manager/references/intake.md".to_string(), "Slots first.".to_string()),
        ("skill://../escape/SKILL.md".to_string(), "never written".to_string()),
        ("file:///etc/passwd".to_string(), "not a skill".to_string()),
    ]));
    let url = serve(Arc::clone(&offered)).await;
    let home = tempfile::tempdir().unwrap();
    let (events, _) = mpsc::channel(64);
    let config = McpServerConfig {
        transport: McpTransport::Http {
            url,
            headers: HashMap::new(),
        },
        timeout_secs: 10,
        ..McpServerConfig::default()
    };
    let manager = McpManager::with_token_store(
        [("connectors".to_string(), config)].into_iter().collect(),
        events,
        false,
        TokenStore::new(home.path().join("t.json")),
    );
    manager.connect("connectors").await.unwrap();

    let dest = home.path().join("served-skills").join("connectors");
    assert_eq!(
        manager
            .sync_served_skills("connectors", &dest)
            .await
            .unwrap(),
        2
    );
    assert!(dest.join("case-manager/SKILL.md").is_file());
    assert_eq!(
        std::fs::read_to_string(dest.join("case-manager/references/intake.md")).unwrap(),
        "Slots first."
    );
    assert!(
        !home.path().join("served-skills/escape").exists(),
        "a path that climbs out is not written"
    );

    // The server stops offering it: the next sync removes it.
    *offered.lock().unwrap() = vec![(
        "skill://review/SKILL.md".to_string(),
        "---\nname: review\ndescription: Review.\n---\nRead.\n".to_string(),
    )];
    assert_eq!(
        manager
            .sync_served_skills("connectors", &dest)
            .await
            .unwrap(),
        1
    );
    assert!(!dest.join("case-manager").exists());
    assert!(dest.join("review/SKILL.md").is_file());
}
