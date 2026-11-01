// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for signing agents in to OAuth
// protected services without leaking or misdirecting tokens for its clients.
// If your team needs expertise in OAuth security for MCP clients then you can
// procure our services by sending an email to info@swedishembedded.com.
//! Signing in to an OAuth-protected MCP server against a local authorization
//! server: the issuer of the authorization response is checked (RFC 9207)
//! and the token is bound to the MCP server (RFC 8707 `resource`).

use std::sync::{Arc, Mutex};

use sven_mcp_client::auth::{sign_in, SignIn, TokenStore};
use sven_mcp_client::McpOAuthConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// What the authorization server saw.
#[derive(Default)]
struct Seen {
    authorize_query: Option<String>,
    token_body: Option<String>,
}

/// One server playing the MCP server's metadata and its authorization
/// server. Its authorize endpoint sends the browser back with `iss` set to
/// `iss_in_response` (its own issuer when `None`).
async fn authorization_server(iss_in_response: Option<String>) -> (String, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Seen::default()));
    let (base, record) = (origin.clone(), Arc::clone(&seen));
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (base, record, iss) = (base.clone(), Arc::clone(&record), iss_in_response.clone());
            tokio::spawn(async move {
                let Some((method, target, body)) = read_request(&mut socket).await else {
                    return;
                };
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let json = |v: serde_json::Value| (200, "application/json", v.to_string(), None);
                let response = match (method.as_str(), path) {
                    ("GET", p) if p.starts_with("/.well-known/oauth-protected-resource") => {
                        json(serde_json::json!({
                            "resource": format!("{base}/mcp"),
                            "authorization_servers": [base],
                        }))
                    }
                    ("GET", p) if p.starts_with("/.well-known/oauth-authorization-server") => {
                        json(serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "registration_endpoint": format!("{base}/register"),
                            "code_challenge_methods_supported": ["S256"],
                            "response_types_supported": ["code"],
                            "authorization_response_iss_parameter_supported": true,
                        }))
                    }
                    ("POST", "/register") => (
                        201,
                        "application/json",
                        serde_json::json!({"client_id": "registered", "redirect_uris": []})
                            .to_string(),
                        None,
                    ),
                    ("GET", "/authorize") => {
                        record.lock().unwrap().authorize_query = Some(query.to_string());
                        let param = |k: &str| {
                            url::form_urlencoded::parse(query.as_bytes())
                                .find(|(key, _)| key == k)
                                .map(|(_, v)| v.into_owned())
                                .unwrap()
                        };
                        let iss = iss.unwrap_or_else(|| base.clone());
                        let location = format!(
                            "{}?code=the-code&state={}&iss={}",
                            param("redirect_uri"),
                            param("state"),
                            url::form_urlencoded::byte_serialize(iss.as_bytes())
                                .collect::<String>()
                        );
                        (302, "text/plain", String::new(), Some(location))
                    }
                    ("POST", "/token") => {
                        record.lock().unwrap().token_body = Some(body);
                        json(serde_json::json!({
                            "access_token": "the-access-token",
                            "token_type": "Bearer",
                            "expires_in": 3600,
                            "refresh_token": "the-refresh-token",
                        }))
                    }
                    _ => (404, "text/plain", String::new(), None),
                };
                write_response(&mut socket, response).await;
            });
        }
    });
    (origin, seen)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> Option<(String, String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
    let length: usize = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + length {
        let n = socket.read(&mut chunk).await.ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..]).to_string();
    Some((method, target, body))
}

async fn write_response(
    socket: &mut tokio::net::TcpStream,
    (status, kind, body, location): (u16, &str, String, Option<String>),
) {
    let location = location.map_or(String::new(), |l| format!("location: {l}\r\n"));
    let answer = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\n{location}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(answer.as_bytes()).await;
}

/// The person's browser: follows the sign-in page to where the
/// authorization server sends it back, and goes there.
fn browser() -> impl Fn(&str) + Send + Sync {
    |auth_url: &str| {
        let auth_url = auth_url.to_string();
        tokio::spawn(async move {
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let answer = http.get(&auth_url).send().await.unwrap();
            let back = answer.headers()["location"].to_str().unwrap().to_string();
            let _ = http.get(back).send().await;
        });
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn run(
    iss_in_response: Option<String>,
) -> (anyhow::Result<()>, TokenStore, String, Arc<Mutex<Seen>>) {
    let (origin, seen) = authorization_server(iss_in_response).await;
    let mcp = format!("{origin}/mcp");
    let dir = tempfile::tempdir().unwrap().keep();
    let store = Arc::new(TokenStore::new(dir.join("mcp-oauth.json")));
    let port = free_port();
    let oauth = McpOAuthConfig {
        redirect_uri: Some(format!("http://127.0.0.1:{port}/callback")),
        callback_port: Some(port),
        ..McpOAuthConfig::default()
    };
    let key = TokenStore::key("srv", &mcp);
    let present = browser();
    let result = sign_in(
        &store,
        SignIn {
            key: &key,
            url: &mcp,
            oauth: Some(&oauth),
            challenge: None,
            present: &present,
        },
    )
    .await;
    let store = Arc::try_unwrap(store).ok().unwrap();
    (result, store, key, seen)
}

#[tokio::test]
async fn a_sign_in_binds_the_token_to_the_server_and_keeps_its_issuer() {
    let (result, store, key, seen) = run(None).await;
    result.unwrap();
    let kept = store.load(&key).unwrap().expect("the sign-in is kept");
    assert_eq!(kept.client_id, "registered");
    assert!(kept
        .issuer
        .as_deref()
        .is_some_and(|i| i.starts_with("http://127.0.0.1:")));
    let seen = seen.lock().unwrap();
    let resource = |s: &str| {
        url::form_urlencoded::parse(s.as_bytes())
            .find(|(k, _)| k == "resource")
            .map(|(_, v)| v.into_owned())
    };
    let authorize = seen.authorize_query.as_deref().unwrap();
    assert!(
        resource(authorize).is_some_and(|r| r.ends_with("/mcp")),
        "{authorize}"
    );
    let token = seen.token_body.as_deref().unwrap();
    assert!(
        resource(token).is_some_and(|r| r.ends_with("/mcp")),
        "{token}"
    );
}

#[tokio::test]
async fn a_response_from_another_issuer_signs_in_nothing() {
    let (result, store, key, seen) = run(Some("https://attacker.example".into())).await;
    let err = format!(
        "{:#}",
        result.expect_err("a mix-up attack must fail the sign-in")
    );
    assert!(err.to_lowercase().contains("issuer"), "{err}");
    assert!(store
        .load(&key)
        .unwrap()
        .is_none_or(|c| c.token_response.is_none()));
    assert!(
        seen.lock().unwrap().token_body.is_none(),
        "the code was never exchanged"
    );
}
