// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sign_in_with` against a scripted local server: discovery, the device
//! endpoints and `whoami`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::{sign_in_with, RemoteLogin};
use crate::device::Pacing;
use uuid::Uuid;

const DEVICE_CODE: &str = "DEVICE-CODE-MUST-STAY-SECRET";
const TOKEN: &str = "sk-minted-token";

/// A canned response: status line, extra headers, JSON body.
type Canned = (u16, &'static str, String);

struct Mock {
    base: String,
    /// `(path, arrival time)` of every request.
    seen: Arc<Mutex<Vec<(String, std::time::Instant)>>>,
    bodies: Arc<Mutex<Vec<String>>>,
}

fn json(status: u16, body: &str) -> Canned {
    (status, "", body.to_string())
}

/// Serves each path's queue in order; the last response repeats.
async fn mock(routes: HashMap<&'static str, VecDeque<Canned>>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    mock_on(listener, routes).await
}

async fn mock_on(
    listener: TcpListener,
    mut routes: HashMap<&'static str, VecDeque<Canned>>,
) -> Mock {
    let base = format!("http://{}", listener.local_addr().unwrap());
    routes.entry("/.well-known/sven-remote").or_insert_with(|| {
        VecDeque::from([json(
            200,
            &format!(
                r#"{{"protocol_version":1,"authorize_url":"{base}/connect/sven",
                "device_authorization_url":"{base}/device","device_token_url":"{base}/device/token",
                "control_url":"{base}/api","openai_base_url":"{base}/v1"}}"#
            ),
        )])
    });
    routes.insert(
        "/api/whoami",
        VecDeque::from([json(
            200,
            r#"{"workspace_id":"6f1b7a52-0a56-4a7e-9a39-7c1c7a2f9d10","workspace_name":"box","account":"Acme AB"}"#,
        )]),
    );
    let routes = Arc::new(Mutex::new(routes));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let (seen2, bodies2) = (seen.clone(), bodies.clone());
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut n = socket.read(&mut buf).await.unwrap();
            loop {
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                let Some((head, body)) = text.split_once("\r\n\r\n") else {
                    n += socket.read(&mut buf[n..]).await.unwrap();
                    continue;
                };
                let length = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if body.len() >= length {
                    break;
                }
                n += socket.read(&mut buf[n..]).await.unwrap();
            }
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = text
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            seen2
                .lock()
                .unwrap()
                .push((path.clone(), std::time::Instant::now()));
            bodies2.lock().unwrap().push(
                text.split_once("\r\n\r\n")
                    .map(|(_, b)| b.to_string())
                    .unwrap_or_default(),
            );
            let (status, headers, body) = {
                let mut routes = routes.lock().unwrap();
                match routes.get_mut(path.as_str()) {
                    Some(queue) if queue.len() > 1 => queue.pop_front().unwrap(),
                    Some(queue) => queue.front().cloned().unwrap(),
                    None => json(404, "{}"),
                }
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\n{headers}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    Mock { base, seen, bodies }
}

fn authorization(expires_in: u32) -> Canned {
    json(
        200,
        &format!(
            r#"{{"device_code":"{DEVICE_CODE}","user_code":"WDJB-MJHT","verification_uri":"https://site/connect/sven","expires_in":{expires_in},"interval":1}}"#
        ),
    )
}

fn error(code: &str) -> Canned {
    json(400, &format!(r#"{{"error":"{code}"}}"#))
}

fn granted() -> Canned {
    json(
        200,
        &format!(r#"{{"token":"{TOKEN}","account":"Acme AB"}}"#),
    )
}

/// One "second" is 20 ms.
const FAST: Pacing = Pacing {
    unit: Duration::from_millis(20),
};

struct Outcome {
    result: anyhow::Result<RemoteLogin>,
    said: Vec<String>,
    stored: Vec<RemoteLogin>,
}

async fn login(mock: &Mock, expires_in_cancel: Option<Duration>) -> Outcome {
    let mut said = Vec::new();
    let mut stored = Vec::new();
    let cancel = async move {
        match expires_in_cancel {
            Some(after) => tokio::time::sleep(after).await,
            None => std::future::pending().await,
        }
    };
    let result = sign_in_with(
        &mock.base,
        "box",
        |line| said.push(line.to_string()),
        FAST,
        cancel,
        |login| {
            stored.push(login.clone());
            Ok(())
        },
    )
    .await;
    Outcome {
        result,
        said,
        stored,
    }
}

fn routes(request: Canned, polls: Vec<Canned>) -> HashMap<&'static str, VecDeque<Canned>> {
    HashMap::from([
        ("/device", VecDeque::from([request])),
        ("/device/token", VecDeque::from(polls)),
    ])
}

fn assert_secret_free(outcome: &Outcome) {
    let shown = outcome.said.join("\n");
    assert!(!shown.contains(DEVICE_CODE));
    if let Err(e) = &outcome.result {
        assert!(!format!("{e:#} {e:?}").contains(DEVICE_CODE));
    }
}

#[tokio::test]
async fn pending_then_success_stores_the_login() {
    let m = mock(routes(
        authorization(600),
        vec![error("authorization_pending"), granted()],
    ))
    .await;
    let out = login(&m, None).await;
    let login = out.result.as_ref().unwrap();
    assert_eq!(login.token, TOKEN);
    assert_eq!(login.account, "Acme AB");
    assert_eq!(login.workspace_name, "box");
    assert_eq!(out.stored.len(), 1);
    let shown = out.said.join("\n");
    assert!(shown.contains("https://site/connect/sven") && shown.contains("WDJB-MJHT"));
    assert_secret_free(&out);
    // The request carried the machine name, polls carried the device code.
    let bodies = m.bodies.lock().unwrap();
    assert!(bodies.iter().any(|b| b.contains(r#""name":"box""#)));
    assert!(bodies.iter().any(|b| b.contains(DEVICE_CODE)));
}

#[tokio::test]
async fn slow_down_lengthens_the_interval() {
    let m = mock(routes(
        authorization(600),
        vec![error("slow_down"), granted()],
    ))
    .await;
    let out = login(&m, None).await;
    out.result.as_ref().unwrap();
    let seen = m.seen.lock().unwrap();
    let polls: Vec<_> = seen.iter().filter(|(p, _)| p == "/device/token").collect();
    assert_eq!(polls.len(), 2);
    // interval 1 -> 6 units after slow_down: at least 6 * 20 ms apart.
    assert!(polls[1].1 - polls[0].1 >= Duration::from_millis(115));
}

#[tokio::test]
async fn denial_shows_the_servers_reason() {
    let m = mock(routes(
        authorization(600),
        vec![json(
            400,
            r#"{"error":"access_denied","error_description":"this account already has 10 keys"}"#,
        )],
    ))
    .await;
    let out = login(&m, None).await;
    let message = format!("{:#}", out.result.as_ref().unwrap_err());
    assert!(
        message.contains("this account already has 10 keys"),
        "{message}"
    );
    assert!(out.stored.is_empty());
    assert_secret_free(&out);
}

#[tokio::test]
async fn plain_denial_and_expiry_are_actionable() {
    let denied = mock(routes(authorization(600), vec![error("access_denied")])).await;
    let message = format!("{:#}", login(&denied, None).await.result.unwrap_err());
    assert!(message.contains("denied"), "{message}");

    let expired = mock(routes(authorization(600), vec![error("expired_token")])).await;
    let out = login(&expired, None).await;
    let message = format!("{:#}", out.result.as_ref().unwrap_err());
    assert!(message.contains("run `sven auth login` again"), "{message}");
    assert_secret_free(&out);
}

#[tokio::test]
async fn the_deadline_ends_the_wait() {
    let m = mock(routes(
        authorization(3),
        vec![error("authorization_pending")],
    ))
    .await;
    let out = login(&m, None).await;
    let message = format!("{:#}", out.result.unwrap_err());
    assert!(message.contains("expired"), "{message}");
}

#[tokio::test]
async fn ctrl_c_ends_the_wait() {
    let m = mock(routes(
        authorization(600),
        vec![error("authorization_pending")],
    ))
    .await;
    let out = login(&m, Some(Duration::from_millis(100))).await;
    let message = format!("{:#}", out.result.unwrap_err());
    assert!(message.contains("cancelled"), "{message}");
    assert!(out.stored.is_empty());
}

#[tokio::test]
async fn a_transient_failure_is_retried_and_a_lasting_one_reported() {
    let flaky = mock(routes(authorization(600), vec![json(503, "{}"), granted()])).await;
    login(&flaky, None).await.result.unwrap();

    let down = mock(routes(authorization(600), vec![json(503, "{}")])).await;
    let out = login(&down, None).await;
    let message = format!("{:#}", out.result.as_ref().unwrap_err());
    assert!(message.contains("lost the server"), "{message}");
    assert_secret_free(&out);
}

#[tokio::test]
async fn a_rate_limited_request_says_how_long_to_wait() {
    let m = mock(routes(
        (429, "retry-after: 42\r\n", "{}".to_string()),
        vec![granted()],
    ))
    .await;
    let message = format!("{:#}", login(&m, None).await.result.unwrap_err());
    assert!(message.contains("42 seconds"), "{message}");
}

#[tokio::test]
async fn a_server_without_the_device_grant_must_be_updated() {
    let old = mock(HashMap::from([(
        "/.well-known/sven-remote",
        VecDeque::from([json(
            200,
            r#"{"protocol_version":1,"authorize_url":"x","control_url":"y","openai_base_url":"z"}"#,
        )]),
    )]))
    .await;
    let out = login_at(&old.base).await;
    let message = format!("{:#}", out.unwrap_err());
    assert!(message.contains("must be updated"), "{message}");
}

async fn login_at(base: &str) -> anyhow::Result<RemoteLogin> {
    sign_in_with(
        base,
        "box",
        |_| {},
        FAST,
        std::future::pending(),
        |_| Ok(()),
    )
    .await
}

fn signed_in_to(openai_base_url: &str) -> RemoteLogin {
    RemoteLogin {
        server: "https://server".into(),
        control_url: "https://server/api/control".into(),
        openai_base_url: openai_base_url.into(),
        token: "minted".into(),
        workspace_id: Uuid::nil(),
        workspace_name: "w".into(),
        account: "a@example.com".into(),
    }
}

/// The sign-in this crate writes is the one the `sven` model provider reads
/// (`sven_model_drivers::sven_service`): its server's models, with its key,
/// wherever that server is.
#[test]
fn a_saved_sign_in_is_the_sven_providers_server_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("remote.yaml");
    let login = signed_in_to("http://localhost:3000/v1");
    std::fs::write(&file, serde_yaml::to_string(&login).unwrap()).unwrap();
    assert_eq!(
        sven_model_drivers::sven_service::signed_in_from(&file),
        Some(sven_model_drivers::sven_service::SignedIn {
            base_url: login.openai_base_url,
            key: login.token,
        })
    );
}

#[test]
fn a_hosted_host_may_connect_to_the_ports_of_its_servers_urls_and_no_others() {
    let mut login = signed_in_to("http://127.0.0.1:3000/v1");
    login.control_url = "https://server:8443/api/control".into();
    assert_eq!(login.server_ports().unwrap(), vec![443, 8443, 3000]);
    login.control_url = "not a url".into();
    assert!(login.server_ports().is_err());
}

#[tokio::test]
async fn discovery_naming_another_origin_for_the_token_is_refused() {
    for field in [
        "control_url",
        "openai_base_url",
        "device_authorization_url",
        "device_token_url",
    ] {
        let mut routes = HashMap::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut document = serde_json::json!({
            "protocol_version": 1, "authorize_url": format!("{base}/connect"),
            "device_authorization_url": format!("{base}/device"),
            "device_token_url": format!("{base}/device/token"),
            "control_url": format!("{base}/api"), "openai_base_url": format!("{base}/v1"),
        });
        document[field] = "http://127.0.0.1:9/elsewhere".into();
        routes.insert(
            "/.well-known/sven-remote",
            VecDeque::from([json(200, &document.to_string())]),
        );
        let m = mock_on(listener, routes).await;
        let message = format!("{:#}", login(&m, None).await.result.unwrap_err());
        assert!(message.contains("same origin"), "{field}: {message}");
        assert!(
            m.seen
                .lock()
                .unwrap()
                .iter()
                .all(|(path, _)| path != "/device"),
            "{field}: no device request was made"
        );
    }
}
