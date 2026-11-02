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
async fn authorization_server(
    iss_in_response: Option<String>,
    registration: bool,
) -> (String, Arc<Mutex<Seen>>) {
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
                        let mut metadata = serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "code_challenge_methods_supported": ["S256"],
                            "response_types_supported": ["code"],
                            "authorization_response_iss_parameter_supported": true,
                        });
                        if registration {
                            metadata["registration_endpoint"] = format!("{base}/register").into();
                        }
                        json(metadata)
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
                    // The MCP endpoint refuses every token it is given.
                    ("POST", "/mcp") => (401, "text/plain", String::new(), Some(String::new())),
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
    // A 401 carries its challenge; anything else with a location redirects.
    let location = match location {
        Some(_) if status == 401 => "www-authenticate: Bearer\r\n".to_string(),
        Some(l) => format!("location: {l}\r\n"),
        None => String::new(),
    };
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
    run_with(iss_in_response, true, None).await.0
}

/// A sign-in against a server that does or does not let clients register,
/// with `client_id` configured or not; and whether a browser was opened.
async fn run_with(
    iss_in_response: Option<String>,
    registration: bool,
    client_id: Option<&str>,
) -> (
    (anyhow::Result<()>, TokenStore, String, Arc<Mutex<Seen>>),
    bool,
) {
    let (origin, seen) = authorization_server(iss_in_response, registration).await;
    let mcp = format!("{origin}/mcp");
    let dir = tempfile::tempdir().unwrap().keep();
    let store = Arc::new(TokenStore::new(dir.join("mcp-oauth.json")));
    let port = free_port();
    let oauth = McpOAuthConfig {
        redirect_uri: Some(format!("http://127.0.0.1:{port}/callback")),
        callback_port: Some(port),
        client_id: client_id.map(str::to_string),
        client_secret: client_id.map(|_| "the-secret".to_string()),
        ..McpOAuthConfig::default()
    };
    let key = TokenStore::key("srv", &mcp);
    let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let follow = browser();
    let flag = Arc::clone(&opened);
    let present = move |url: &str| {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        follow(url);
    };
    let result = sign_in(
        &store,
        SignIn {
            key: &key,
            url: &mcp,
            oauth: Some(&oauth),
            challenge: None,
            present: &present,
            pasted: None,
        },
    )
    .await;
    let store = Arc::try_unwrap(store).ok().unwrap();
    let opened = opened.load(std::sync::atomic::Ordering::SeqCst);
    ((result, store, key, seen), opened)
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

/// A server that refuses the sign-in sven kept for it: the sign-in is
/// forgotten and the person asked for a new one, rather than the browser
/// opening in a loop or the server failing for good.
#[tokio::test]
async fn a_refused_sign_in_is_forgotten_and_a_new_one_asked_for() {
    use sven_mcp_client::{McpEvent, McpManager, McpServerConfig, McpTransport, ServerStatus};
    let (origin, _) = authorization_server(None, true).await;
    let mcp = format!("{origin}/mcp");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp-oauth.json");
    let key = TokenStore::key("srv", &mcp);
    std::fs::write(
        &path,
        serde_json::json!({ key.clone(): {
            "client_id": "registered",
            "token_response": {"access_token": "stale", "token_type": "bearer"},
            "issuer": origin,
        }})
        .to_string(),
    )
    .unwrap();
    let config = McpServerConfig {
        transport: McpTransport::Http {
            url: mcp.clone(),
            headers: Default::default(),
        },
        timeout_secs: 10,
        ..McpServerConfig::default()
    };
    let (tx, mut events) = tokio::sync::mpsc::channel(16);
    let manager = McpManager::with_token_store(
        [("srv".to_string(), config)].into(),
        tx,
        true,
        TokenStore::new(path.clone()),
    );
    assert!(manager.connect("srv").await.is_err());
    let asked = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Some(McpEvent::AuthRequired { server, .. }) => break server,
                Some(McpEvent::AuthStarted { .. }) => panic!("the browser must not open again"),
                Some(_) => {}
                None => panic!("no AuthRequired"),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(asked, "srv");
    assert!(TokenStore::new(path).load(&key).unwrap().is_none());
    assert!(matches!(
        manager.server_statuses().await[0].status,
        ServerStatus::NeedsAuth { .. }
    ));
}

/// A server that lets nobody register (GitHub's does not) cannot be signed
/// in to without a client of one's own: refused before any browser opens,
/// saying what works instead.
#[tokio::test]
async fn a_server_that_lets_nobody_register_is_refused_with_what_to_do() {
    let ((result, _, _, seen), opened) = run_with(None, false, None).await;
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        !opened,
        "a browser was opened for a sign-in that cannot work"
    );
    assert!(seen.lock().unwrap().authorize_query.is_none());
    for hint in ["--client-id", "--header", "Connectors"] {
        assert!(message.contains(hint), "{hint} missing from: {message}");
    }
}

/// With a client of one's own configured, that client signs in.
#[tokio::test]
async fn a_configured_client_signs_in_where_nobody_may_register() {
    let ((result, _, _, seen), opened) = run_with(None, false, Some("own-app")).await;
    result.unwrap();
    assert!(opened);
    let query = seen.lock().unwrap().authorize_query.clone().unwrap();
    assert!(query.contains("client_id=own-app"), "{query}");
}
