// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for connecting agents to the
// services their people use without the agents holding any sign-in, for its
// clients. If your team needs expertise in MCP and OAuth integration then
// you can procure our services by sending an email to
// info@swedishembedded.com.
//! Adding a server to the signed-in person's account (`sven mcp add` while
//! signed in): the machine's key goes to the token endpoint alone, the
//! account is asked with the short-lived token it traded, and what comes
//! back is the service's sign-in page, or the account's reason for refusing.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One request as the account saw it: its line, headers and body.
#[derive(Clone, Debug)]
struct Seen {
    line: String,
    head: String,
    body: String,
}

/// An account that trades `key` for a token and answers `/api/mcp/connect`
/// with `connect` (status, body).
async fn account(connect: (u16, &'static str)) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = Arc::clone(&log);
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
                let head = String::from_utf8_lossy(&buf[..end]).to_string();
                let length: usize = head
                    .lines()
                    .filter_map(|l| l.split_once(':'))
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .and_then(|(_, v)| v.trim().parse().ok())
                    .unwrap_or(0);
                while buf.len() < end + length {
                    let n = socket.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let line = head.lines().next().unwrap_or_default().to_string();
                let body = String::from_utf8_lossy(&buf[end..]).to_string();
                log.lock().unwrap().push(Seen {
                    line: line.clone(),
                    head: head.to_lowercase(),
                    body,
                });
                let (status, answer) = if line.starts_with("POST /api/mcp/token ") {
                    (
                        200,
                        r#"{"access_token":"short-lived","token_type":"Bearer","expires_in":300}"#,
                    )
                } else if line.starts_with("POST /api/mcp/connect ") {
                    connect
                } else {
                    (404, "{}")
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (origin, seen)
}

fn exchange(origin: &str) -> sven_mcp_client::TokenExchange {
    sven_mcp_client::TokenExchange {
        url: format!("{origin}/api/mcp/token"),
        key: "machine-key".into(),
    }
}

#[tokio::test]
async fn the_account_adds_it_with_a_traded_token_and_gives_back_the_sign_in_page() {
    let (origin, seen) = account((
        200,
        r#"{"connector":{"id":"c1","name":"GitHub"},"signInUrl":"https://github.com/login/oauth/authorize?state=s"}"#,
    ))
    .await;
    let connection = sven_mcp_client::connect_account(
        &exchange(&origin),
        &format!("{origin}/api/mcp"),
        "GitHub",
        "https://api.githubcopilot.com/mcp/",
    )
    .await
    .unwrap();
    assert_eq!(connection.connector.name, "GitHub");
    assert_eq!(
        connection.sign_in_url,
        "https://github.com/login/oauth/authorize?state=s"
    );

    let seen = seen.lock().unwrap().clone();
    let [token, connect] = seen.as_slice() else {
        panic!("expected a token trade then the connect, saw {seen:?}");
    };
    assert!(token.line.starts_with("POST /api/mcp/token "));
    assert!(token.head.contains("authorization: bearer machine-key"));
    assert!(connect.line.starts_with("POST /api/mcp/connect "));
    assert!(
        connect.head.contains("authorization: bearer short-lived"),
        "the account is asked with the traded token"
    );
    assert!(
        !connect.head.contains("machine-key"),
        "the key goes to the token endpoint alone"
    );
    let body: serde_json::Value = serde_json::from_str(&connect.body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"name": "GitHub", "url": "https://api.githubcopilot.com/mcp/"})
    );
}

#[tokio::test]
async fn a_refusal_says_the_accounts_reason() {
    let (origin, _) = account((
        400,
        r#"{"error":"only a computer you connected adds a connector"}"#,
    ))
    .await;
    let refused = sven_mcp_client::connect_account(
        &exchange(&origin),
        &format!("{origin}/api/mcp/"),
        "GitHub",
        "https://api.githubcopilot.com/mcp/",
    )
    .await
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("only a computer you connected adds a connector"),
        "{refused:#}"
    );
}
