// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for making agent setups that fail
// early and explain why for its clients. If your team needs expertise in
// reliable LLM provider integration then you can procure our services by
// sending an email to info@swedishembedded.com.
//! `check_provider` answers whether the configured model can be used, and
//! says what to change when it cannot.

mod common;

use common::{list, Server};
use sven_model_drivers::{check_provider, ModelConfig, ProviderReport, Verdict};
use tokio::net::TcpListener;

fn verdict(report: &ProviderReport, name: &str) -> Verdict {
    report
        .checks
        .iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("no check {name}: {:?}", report.checks))
        .verdict
}

#[tokio::test]
async fn a_served_model_with_tools_is_ready() {
    let server = Server::start(list(vec![serde_json::json!({
        "id": "resident", "capabilities": ["chat", "tools"]
    })]))
    .await;
    let report = check_provider(&server.config("resident")).await;
    assert!(report.ready(), "{report:?}");
    assert_eq!(verdict(&report, "model"), Verdict::Ok);
    assert_eq!(verdict(&report, "tools"), Verdict::Ok);
}

#[tokio::test]
async fn a_server_that_does_not_answer_fails_the_endpoint_check() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: "m".into(),
        base_url: Some(format!("http://{}/v1", closed.local_addr().unwrap())),
        ..ModelConfig::default()
    };
    drop(closed);
    let report = check_provider(&cfg).await;
    assert!(!report.ready());
    assert_eq!(verdict(&report, "endpoint"), Verdict::Fail);
    let endpoint = report.checks.iter().find(|c| c.name == "endpoint").unwrap();
    assert!(endpoint.fix.is_some(), "a failure says what to do");
}

#[tokio::test]
async fn a_model_the_server_does_not_serve_is_named_with_what_it_does() {
    let server = Server::start(list(vec![serde_json::json!({"id": "other"})])).await;
    let report = check_provider(&server.config("wanted")).await;
    assert!(!report.ready());
    let model = report.checks.iter().find(|c| c.name == "model").unwrap();
    assert_eq!(model.verdict, Verdict::Fail);
    assert!(model.detail.contains("wanted") && model.detail.contains("other"));
}

#[tokio::test]
async fn a_model_without_tool_calling_warns_but_stays_ready() {
    let server = Server::start(list(vec![serde_json::json!({
        "id": "plain", "capabilities": ["chat"]
    })]))
    .await;
    let report = check_provider(&server.config("plain")).await;
    assert_eq!(verdict(&report, "tools"), Verdict::Warn);
    assert!(report.ready(), "agent mode may still use it without tools");
}

#[tokio::test]
async fn a_hosted_provider_is_not_probed_and_a_missing_key_is_the_failure() {
    let cfg = ModelConfig {
        provider: "anthropic".into(),
        name: "claude-sonnet-4-6".into(),
        api_key: None,
        api_key_env: Some("SVEN_TEST_NO_SUCH_KEY_VAR".into()),
        ..ModelConfig::default()
    };
    let report = check_provider(&cfg).await;
    assert!(!report.ready());
    assert_eq!(verdict(&report, "provider"), Verdict::Fail);
    assert!(
        report.checks.iter().all(|c| c.name != "endpoint"),
        "nothing is probed once the provider cannot be built"
    );
}

#[tokio::test]
async fn the_reported_endpoint_carries_no_credentials() {
    let cfg = ModelConfig {
        provider: "ollama".into(),
        name: "m".into(),
        base_url: Some("http://user:hunter2@127.0.0.1:9/v1?token=hunter2".into()),
        ..ModelConfig::default()
    };
    let report = check_provider(&cfg).await;
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("hunter2"), "{text}");
}
