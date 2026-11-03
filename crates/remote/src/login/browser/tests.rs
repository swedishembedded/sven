// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The code grant against a scripted local server. The "browser" is the
//! terminal double: opening a link makes it GET the redirect the real
//! authorization page would send the person back to.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::net::TcpListener;

use super::{over_ssh, Terminal};
use crate::device::Pacing;
use crate::login::pkce::challenge_of;
use crate::login::tests::{json, mock_on, Canned, Mock};
use crate::login::{sign_in_with, RemoteLogin};

const TOKEN: &str = "sk-minted-token";

/// What the browser does when the link is opened.
#[derive(Clone)]
enum Browser {
    /// Cannot be opened here.
    Absent,
    /// The person approves: returns with `code` after each of `states`
    /// (`None` is this sign-in's own state), in order.
    Returns {
        code: &'static str,
        states: Vec<Option<&'static str>>,
    },
    /// The person declines.
    Declines,
}

struct Double {
    paste: bool,
    browser: Browser,
    pasted: Option<&'static str>,
    opened: Arc<Mutex<Vec<String>>>,
    /// Status of each callback request the browser made.
    callback_statuses: Arc<Mutex<Vec<u16>>>,
}

impl Double {
    fn new(paste: bool, browser: Browser, pasted: Option<&'static str>) -> Self {
        Self {
            paste,
            browser,
            pasted,
            opened: Arc::default(),
            callback_statuses: Arc::default(),
        }
    }
}

#[async_trait(?Send)]
impl Terminal for Double {
    fn prefers_paste(&self) -> bool {
        self.paste
    }

    fn open_browser(&self, url: &str) -> bool {
        self.opened.lock().unwrap().push(url.to_string());
        let query: HashMap<String, String> = reqwest::Url::parse(url)
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect();
        let redirect = query["redirect_uri"].clone();
        let state = query["state"].clone();
        let statuses = self.callback_statuses.clone();
        let (code, states) = match self.browser.clone() {
            Browser::Absent => return false,
            Browser::Returns { code, states } => (Some(code), states),
            Browser::Declines => (None, vec![None]),
        };
        tokio::spawn(async move {
            for given in states {
                let state = given.map_or(state.clone(), str::to_string);
                let tail = match code {
                    Some(code) => format!("code={code}&state={state}"),
                    None => format!("error=access_denied&state={state}"),
                };
                let response = reqwest::get(format!("{redirect}?{tail}")).await.unwrap();
                statuses.lock().unwrap().push(response.status().as_u16());
            }
        });
        true
    }

    async fn read_code(&mut self) -> anyhow::Result<String> {
        self.pasted
            .map(|code| code.trim().to_string())
            .ok_or_else(|| anyhow::anyhow!("no code was entered (end of input)"))
    }
}

struct Server {
    mock: Mock,
}

async fn server(document: impl FnOnce(&str) -> serde_json::Value, token: Canned) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let routes = HashMap::from([
        (
            "/.well-known/sven-remote",
            VecDeque::from([json(200, &document(&base).to_string())]),
        ),
        ("/token", VecDeque::from([token])),
    ]);
    Server {
        mock: mock_on(listener, routes).await,
    }
}

fn full_document(base: &str) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": 1, "authorize_url": format!("{base}/connect"),
        "authorization_endpoint": format!("{base}/connect/sven/authorize"),
        "token_endpoint": format!("{base}/token"),
        "code_display_uri": format!("{base}/connect/sven/code"),
        "control_url": format!("{base}/api"), "openai_base_url": format!("{base}/v1"),
    })
}

fn minted() -> Canned {
    json(
        200,
        &format!(r#"{{"token":"{TOKEN}","account":"Acme AB"}}"#),
    )
}

struct Outcome {
    result: anyhow::Result<RemoteLogin>,
    said: Vec<String>,
}

async fn login(server: &Server, terminal: &mut Double) -> Outcome {
    let mut said = Vec::new();
    let result = sign_in_with(
        &server.mock.base,
        "build box",
        |line| said.push(line.to_string()),
        terminal,
        Pacing::default(),
        std::future::pending(),
        |_| Ok(()),
    )
    .await;
    Outcome { result, said }
}

/// The JSON bodies the token endpoint received.
fn token_requests(server: &Server) -> Vec<serde_json::Value> {
    let seen = server.mock.seen.lock().unwrap();
    let bodies = server.mock.bodies.lock().unwrap();
    seen.iter()
        .zip(bodies.iter())
        .filter(|((path, _), _)| path == "/token")
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect()
}

fn query_of(url: &str) -> HashMap<String, String> {
    reqwest::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[tokio::test]
async fn the_browser_returns_to_the_loopback_port_and_the_code_is_redeemed_with_the_verifier() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(
        false,
        Browser::Returns {
            code: "one-time",
            states: vec![None],
        },
        None,
    );
    let out = login(&s, &mut terminal).await;
    let login = out.result.unwrap();
    assert_eq!(
        (login.token.as_str(), login.account.as_str()),
        (TOKEN, "Acme AB")
    );

    let opened = terminal.opened.lock().unwrap().clone();
    assert_eq!(opened.len(), 1);
    let query = query_of(&opened[0]);
    assert!(opened[0].starts_with(&format!("{}/connect/sven/authorize?", s.mock.base)));
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "sven");
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(query["name"], "build box");
    assert!(query["redirect_uri"].starts_with("http://127.0.0.1:"));
    assert!(query["redirect_uri"].ends_with("/callback"));
    assert!(
        out.said.iter().any(|line| *line == opened[0]) || out.said.join("\n").contains(&opened[0]),
        "the link is always printed: {:?}",
        out.said
    );

    let requests = token_requests(&s);
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent["grant_type"], "authorization_code");
    assert_eq!(sent["code"], "one-time");
    assert_eq!(sent["client_id"], "sven");
    assert_eq!(sent["redirect_uri"], query["redirect_uri"]);
    assert_eq!(
        challenge_of(sent["code_verifier"].as_str().unwrap()),
        query["code_challenge"]
    );
    assert_eq!(*terminal.callback_statuses.lock().unwrap(), vec![200]);
    let head = s.mock.heads.lock().unwrap();
    assert!(head.iter().any(
        |h| h.starts_with("POST /token") && h.to_ascii_lowercase().contains("application/json")
    ));
}

#[tokio::test]
async fn a_return_with_another_state_is_refused_and_never_exchanged() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(
        false,
        Browser::Returns {
            code: "forged-or-real",
            states: vec![Some("somebody-elses-state"), None],
        },
        None,
    );
    login(&s, &mut terminal).await.result.unwrap();
    assert_eq!(*terminal.callback_statuses.lock().unwrap(), vec![400, 200]);
    assert_eq!(token_requests(&s).len(), 1);
}

#[tokio::test]
async fn a_declined_sign_in_stops_without_exchanging_anything() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(false, Browser::Declines, None);
    let message = format!("{:#}", login(&s, &mut terminal).await.result.unwrap_err());
    assert!(message.contains("access denied"), "{message}");
    assert!(token_requests(&s).is_empty());
}

#[tokio::test]
async fn without_a_browser_the_code_is_pasted_and_bound_to_the_code_page() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(true, Browser::Absent, Some("  pasted-code \n"));
    let out = login(&s, &mut terminal).await;
    out.result.unwrap();
    assert!(terminal.opened.lock().unwrap().is_empty());

    let base = &s.mock.base;
    assert_eq!(out.said.len(), 5, "{:?}", out.said);
    assert_eq!(
        out.said[0],
        format!("Open this link in a browser signed in to {base}:")
    );
    assert!(out.said[2].starts_with(&format!("{base}/connect/sven/authorize?")));
    assert_eq!(
        out.said[4],
        "Then paste the code it shows here and press Enter:"
    );
    let query = query_of(&out.said[2]);
    let code_page = format!("{base}/connect/sven/code");
    assert_eq!(query["redirect_uri"], code_page);

    let requests = token_requests(&s);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["code"], "pasted-code");
    assert_eq!(requests[0]["redirect_uri"], code_page);
    assert_eq!(
        challenge_of(requests[0]["code_verifier"].as_str().unwrap()),
        query["code_challenge"]
    );
}

#[tokio::test]
async fn a_browser_that_will_not_open_turns_the_sign_in_into_a_pasted_code() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(false, Browser::Absent, Some("pasted"));
    login(&s, &mut terminal).await.result.unwrap();
    assert_eq!(terminal.opened.lock().unwrap().len(), 1);
    assert_eq!(
        token_requests(&s)[0]["redirect_uri"],
        format!("{}/connect/sven/code", s.mock.base)
    );
}

#[tokio::test]
async fn a_server_without_a_code_page_cannot_sign_in_a_headless_machine() {
    let s = server(
        |base| {
            let mut document = full_document(base);
            document.as_object_mut().unwrap().remove("code_display_uri");
            document
        },
        minted(),
    )
    .await;
    let mut terminal = Double::new(true, Browser::Absent, Some("x"));
    let message = format!("{:#}", login(&s, &mut terminal).await.result.unwrap_err());
    assert!(message.contains("where a browser opens"), "{message}");
    assert!(token_requests(&s).is_empty());
}

#[tokio::test]
async fn a_refused_code_says_why_and_is_not_retried() {
    for (body, expected) in [
        (
            r#"{"error":"invalid_grant"}"#,
            "wrong, expired or already used",
        ),
        (r#"{"error":"access_denied"}"#, "denied in the browser"),
        (r#"{"error":"invalid_request"}"#, "update sven"),
        (
            r#"{"error":"invalid_grant","error_description":"this code was already used"}"#,
            "this code was already used",
        ),
    ] {
        let s = server(full_document, json(400, body)).await;
        let mut terminal = Double::new(true, Browser::Absent, Some("c"));
        let message = format!("{:#}", login(&s, &mut terminal).await.result.unwrap_err());
        assert!(message.contains(expected), "{body}: {message}");
        assert_eq!(token_requests(&s).len(), 1, "{body}");
    }
}

#[tokio::test]
async fn new_endpoints_on_another_origin_are_refused_before_anything_is_sent() {
    for field in [
        "authorization_endpoint",
        "token_endpoint",
        "code_display_uri",
    ] {
        let s = server(
            |base| {
                let mut document = full_document(base);
                document[field] = "http://127.0.0.1:9/elsewhere".into();
                document
            },
            minted(),
        )
        .await;
        let mut terminal = Double::new(true, Browser::Absent, Some("c"));
        let message = format!("{:#}", login(&s, &mut terminal).await.result.unwrap_err());
        assert!(message.contains("same origin"), "{field}: {message}");
        assert!(token_requests(&s).is_empty(), "{field}");
    }
}

#[test]
fn a_remote_shell_prefers_a_pasted_code() {
    let with = |set: &'static str| move |name: &str| (name == set).then(|| "10.0.0.1".to_string());
    assert!(over_ssh(with("SSH_CONNECTION")));
    assert!(over_ssh(with("SSH_TTY")));
    assert!(!over_ssh(|_| None));
    assert!(!over_ssh(|_| Some(String::new())));
}

/// Waiting for a callback that never comes ends with the sign-in, not never.
#[tokio::test(start_paused = true)]
async fn ctrl_c_ends_the_wait_for_the_browser() {
    let s = server(full_document, minted()).await;
    let mut terminal = Double::new(
        false,
        Browser::Returns {
            code: "never",
            states: vec![],
        },
        None,
    );
    let mut said = Vec::new();
    let result = sign_in_with(
        &s.mock.base,
        "box",
        |line| said.push(line.to_string()),
        &mut terminal,
        Pacing::default(),
        tokio::time::sleep(Duration::from_secs(5)),
        |_| Ok(()),
    )
    .await;
    assert!(format!("{:#}", result.unwrap_err()).contains("cancelled"));
}
