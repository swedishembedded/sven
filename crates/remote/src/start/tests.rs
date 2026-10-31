// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The start-credential exchange against the scripted local server of the
//! device sign-in tests.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use super::{
    exchange_start_credential_with, read_login_file, Backoff, LoginFile, StartCredentialRefused,
};
use crate::login::tests::{json, mock, Mock};
use crate::login::RemoteLogin;

const CREDENTIAL: &str = "START-CREDENTIAL-MUST-STAY-SECRET";
const TOKEN: &str = "sk-started-token";

const FAST: Backoff = Backoff {
    first: Duration::from_millis(10),
    longest: Duration::from_millis(40),
    total: Duration::from_millis(200),
};

fn started() -> crate::login::tests::Canned {
    json(
        200,
        &format!(
            r#"{{"token":"{TOKEN}","workspace_id":"6f1b7a52-0a56-4a7e-9a39-7c1c7a2f9d10",
            "workspace_name":"box","account":"Acme AB","expires_at":"later"}}"#
        ),
    )
}

async fn exchange(mock: &Mock) -> anyhow::Result<RemoteLogin> {
    let file = LoginFile::new(&mock.base, CREDENTIAL);
    exchange_start_credential_with(&file, FAST, std::future::pending()).await
}

fn start_requests(mock: &Mock) -> usize {
    mock.seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path == "/api/start")
        .count()
}

/// Nothing an outcome shows may carry the credential.
fn assert_secret_free(result: &anyhow::Result<RemoteLogin>) {
    if let Err(e) = result {
        assert!(!format!("{e:#} {e:?}").contains(CREDENTIAL));
    }
}

#[tokio::test]
async fn a_valid_credential_yields_an_in_memory_login_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login.yaml");
    std::fs::write(&path, "server: unused\nstart_credential: x\n").unwrap();

    let m = mock(HashMap::from([("/api/start", VecDeque::from([started()]))])).await;
    let result = exchange(&m).await;
    assert_secret_free(&result);
    let login = result.unwrap();
    assert_eq!(login.token, TOKEN);
    assert_eq!(login.workspace_name, "box");
    assert_eq!(login.account, "Acme AB");
    assert_eq!(login.server, m.base);
    assert_eq!(login.control_url, format!("{}/api", m.base));
    assert_eq!(login.openai_base_url, format!("{}/v1", m.base));
    assert_eq!(
        login.workspace_id.to_string(),
        "6f1b7a52-0a56-4a7e-9a39-7c1c7a2f9d10"
    );

    // The credential went as a bearer token with an empty JSON body.
    let heads = m.heads.lock().unwrap();
    let head = heads
        .iter()
        .find(|h| h.starts_with("POST /api/start"))
        .unwrap();
    assert!(head.contains(&format!("Bearer {CREDENTIAL}")), "{head}");
    assert_eq!(m.bodies.lock().unwrap().last().unwrap(), "{}");

    // The exchange touches no file: the login file is the directory's only
    // content, as it was.
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, ["login.yaml"]);
}

#[tokio::test]
async fn a_refused_credential_says_so_and_is_not_retried() {
    let m = mock(HashMap::from([(
        "/api/start",
        VecDeque::from([json(401, r#"{"error":"used"}"#)]),
    )]))
    .await;
    let result = exchange(&m).await;
    assert_secret_free(&result);
    let e = result.unwrap_err();
    assert!(e.downcast_ref::<StartCredentialRefused>().is_some());
    let message = format!("{e:#}");
    assert!(message.contains("start credential refused"), "{message}");
    assert!(!message.contains("sven auth login"), "{message}");
    assert_eq!(start_requests(&m), 1);
}

#[tokio::test]
async fn a_discovery_document_on_another_origin_is_refused_before_the_credential_is_sent() {
    let m = mock(HashMap::from([(
        "/.well-known/sven-remote",
        VecDeque::from([json(
            200,
            r#"{"protocol_version":1,"authorize_url":"https://evil.example/connect/sven",
            "control_url":"https://evil.example/api","openai_base_url":"https://evil.example/v1"}"#,
        )]),
    )]))
    .await;
    let result = exchange(&m).await;
    assert_secret_free(&result);
    let message = format!("{:#}", result.unwrap_err());
    assert!(message.contains("same origin"), "{message}");
    assert_eq!(start_requests(&m), 0);
}

#[tokio::test]
async fn a_transient_failure_is_retried_until_it_succeeds() {
    let m = mock(HashMap::from([(
        "/api/start",
        VecDeque::from([json(503, "{}"), json(502, "{}"), started()]),
    )]))
    .await;
    let result = exchange(&m).await;
    assert_secret_free(&result);
    assert_eq!(result.unwrap().token, TOKEN);
    assert_eq!(start_requests(&m), 3);
}

#[tokio::test]
async fn a_lasting_failure_is_reported_after_bounded_attempts() {
    let m = mock(HashMap::from([(
        "/api/start",
        VecDeque::from([json(503, "{}")]),
    )]))
    .await;
    let result = exchange(&m).await;
    assert_secret_free(&result);
    let message = format!("{:#}", result.unwrap_err());
    assert!(
        message.contains("503") && message.contains("in time"),
        "{message}"
    );
    let attempts = start_requests(&m);
    assert!((3..=30).contains(&attempts), "{attempts}");
}

#[test]
fn the_login_file_is_read_strictly_and_never_shows_the_credential() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, text).unwrap();
        path
    };
    let good = read_login_file(&write(
        "good.yaml",
        &format!("server: https://s.example\nstart_credential: {CREDENTIAL}\n"),
    ))
    .unwrap();
    assert_eq!(good.server, "https://s.example");
    assert!(!format!("{good:?}").contains(CREDENTIAL));

    for (name, text) in [
        (
            "extra.yaml",
            format!("server: s\nstart_credential: {CREDENTIAL}\nmore: 1\n"),
        ),
        ("missing.yaml", "server: s\n".to_string()),
        (
            "empty.yaml",
            "server: s\nstart_credential: ' '\n".to_string(),
        ),
        (
            "big.yaml",
            format!("server: s\nstart_credential: {}\n", "a".repeat(70_000)),
        ),
    ] {
        let e = read_login_file(&write(name, &text)).unwrap_err();
        assert!(!format!("{e:#}").contains(CREDENTIAL), "{name}");
    }
    assert!(read_login_file(&dir.path().join("absent.yaml")).is_err());
}
