// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Failure mapping, deadlines, discovery, the validated-only guard,
//! concurrency and caching, against a loopback provider.

use std::sync::Arc;
use std::time::Duration;

use sven_decision::brain::{BrainProvider, BrainSettings, MinimumQuality, ModelState};
use sven_decision::{
    DecisionProvider, DecisionRequest, DecisionResult, ErrorClass, Question, QuestionKind,
};

use crate::contract::{provider, recorded_request};
use crate::support::{self, closed, silent, FakeBrain, Reply};

const LAYA: &str = "convaiinnovations/laya";

async fn class_of(status: u16, body: &str) -> ErrorClass {
    let body = body.to_string();
    let brain = FakeBrain::start(move |r| {
        if r.action() == "decide" {
            Reply::status(status, &body)
        } else {
            support::recorded_reply(r)
        }
    })
    .await;
    provider(&brain.url, LAYA)
        .decide(&recorded_request())
        .await
        .expect_err("a failure")
        .class()
}

#[tokio::test]
async fn every_failure_the_provider_reports_maps_to_one_error_class() {
    let table = [
        (
            404,
            include_str!("../fixtures/brain/error_not_installed.404.json"),
            ErrorClass::ModelNotInstalled,
        ),
        (
            500,
            include_str!("../fixtures/brain/error_failed_to_load.500.json"),
            ErrorClass::FailedToLoad,
        ),
        (
            500,
            include_str!("../fixtures/brain/error_backend_failure.500.json"),
            ErrorClass::InvalidResponse,
        ),
        (
            422,
            include_str!("../fixtures/brain/error_unsupported.422.json"),
            ErrorClass::UnsupportedCapability,
        ),
        (
            422,
            include_str!("../fixtures/brain/error_invalid_request.422.json"),
            ErrorClass::InvalidRequest,
        ),
        (
            504,
            include_str!("../fixtures/brain/error_timeout.504.json"),
            ErrorClass::Timeout,
        ),
        (
            401,
            include_str!("../fixtures/brain/error_unauthorized.401.json"),
            ErrorClass::Unreachable,
        ),
        (
            503,
            include_str!("../fixtures/brain/error_saturated.503.json"),
            ErrorClass::Unreachable,
        ),
        (200, "not json", ErrorClass::InvalidResponse),
        (
            200,
            r#"{"outputs": {"answers": []}}"#,
            ErrorClass::InvalidResponse,
        ),
        (502, "<html>bad gateway</html>", ErrorClass::InvalidResponse),
    ];
    for (status, body, expected) in table {
        assert_eq!(class_of(status, body).await, expected, "{status} {body}");
    }
}

#[tokio::test]
async fn an_answer_missing_for_a_question_is_an_invalid_response() {
    let mut doc: serde_json::Value = serde_json::from_str(support::DECIDE).unwrap();
    doc["outputs"]["answers"].as_array_mut().unwrap().pop();
    let body = doc.to_string();
    assert_eq!(class_of(200, &body).await, ErrorClass::InvalidResponse);
}

#[tokio::test]
async fn nothing_listening_is_unreachable() {
    let err = provider(&closed().await, LAYA)
        .decide(&recorded_request())
        .await
        .expect_err("unreachable");
    assert_eq!(err.class(), ErrorClass::Unreachable);
}

#[tokio::test]
async fn a_provider_that_never_answers_times_out_at_the_deadline() {
    let request = DecisionRequest {
        deadline: Duration::from_millis(150),
        ..recorded_request()
    };
    let started = std::time::Instant::now();
    let err = provider(&silent().await, LAYA)
        .decide(&request)
        .await
        .expect_err("timeout");
    assert_eq!(err.class(), ErrorClass::Timeout);
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_base_url_with_or_without_v1_or_a_slash_reaches_the_run_route() {
    let brain = FakeBrain::recorded().await;
    for suffix in ["", "/", "/v1", "/v1/"] {
        provider(&format!("{}{suffix}", brain.url), LAYA)
            .decide(&recorded_request())
            .await
            .unwrap();
    }
    assert!(brain.received().iter().all(|r| r.path == "/v1/run"));
}

#[tokio::test]
async fn the_api_key_is_sent_as_a_bearer_token_only_when_configured() {
    let brain = FakeBrain::recorded().await;
    let keyed = BrainProvider::new(BrainSettings {
        model: LAYA.into(),
        api_key: Some("secret".into()),
        ..BrainSettings::new(&brain.url)
    })
    .unwrap();
    keyed.decide(&recorded_request()).await.unwrap();
    assert!(brain
        .received()
        .iter()
        .all(|r| r.authorization.as_deref() == Some("Bearer secret")));

    let plain = FakeBrain::recorded().await;
    provider(&plain.url, LAYA)
        .decide(&recorded_request())
        .await
        .unwrap();
    assert!(plain.received().iter().all(|r| r.authorization.is_none()));
}

#[tokio::test]
async fn discovery_reports_each_model_state_validation_and_checkpoint_without_loading() {
    let brain = FakeBrain::recorded().await;
    let models = provider(&brain.url, "auto")
        .models(Duration::from_secs(2))
        .await
        .unwrap();
    let by_id = |id: &str| models.iter().find(|m| m.id == id).unwrap();

    let laya = by_id(LAYA);
    assert_eq!(laya.state, ModelState::Ready);
    assert!(laya.validated);
    assert_eq!(laya.validation_profile.as_deref(), Some("ticket-routing"));
    assert_eq!(laya.max_input_tokens, Some(512));
    assert_eq!(
        laya.question_types,
        [
            QuestionKind::Choice,
            QuestionKind::Score,
            QuestionKind::Probability
        ]
    );
    assert!(laya
        .checkpoint
        .as_deref()
        .unwrap()
        .starts_with("sha256:1111"));

    assert!(!by_id("acme/triage").validated);
    let gone = by_id("acme/gone");
    assert_eq!(gone.state, ModelState::NotInstalled);
    assert_eq!(gone.hint.as_deref(), Some("run `brain pull acme/gone`"));
    assert_eq!(by_id("acme/broken").state, ModelState::Incompatible);
    assert_eq!(brain.calls_of("decide"), 0);
}

#[tokio::test]
async fn readiness_uses_the_cheap_queries_and_never_loads_the_model() {
    let brain = FakeBrain::recorded().await;
    let ready = provider(&brain.url, "auto")
        .readiness(Duration::from_secs(2))
        .await
        .unwrap();
    assert_eq!(ready.id, LAYA);
    assert_eq!(brain.calls_of("decide"), 0);
    assert_eq!(brain.calls_of("select"), 1);
}

#[tokio::test]
async fn auto_is_left_to_the_provider_and_resolved_once() {
    let brain = FakeBrain::recorded().await;
    let p = provider(&brain.url, "auto");
    p.decide(&recorded_request()).await.unwrap();
    p.decide(&recorded_request()).await.unwrap();
    let decides: Vec<_> = brain
        .received()
        .into_iter()
        .filter(|r| r.action() == "decide")
        .collect();
    assert!(decides.iter().all(|r| r.model() == LAYA));
    assert_eq!(
        brain.calls_of("select"),
        1,
        "the selection is not asked per decision"
    );
}

#[tokio::test]
async fn auto_with_nothing_selected_says_no_model_is_installed() {
    let brain = FakeBrain::start(|r| match r.action() {
        "select" => Reply::ok(support::SELECT_NONE),
        "models" => Reply::ok(r#"{"outputs": {"models": []}}"#),
        _ => Reply::status(500, "{}"),
    })
    .await;
    let err = provider(&brain.url, "auto")
        .readiness(Duration::from_secs(2))
        .await
        .expect_err("nothing selected");
    assert_eq!(err.class(), ErrorClass::ModelNotInstalled);
    assert!(
        err.to_string().contains("no decision model is installed"),
        "{err}"
    );
}

#[tokio::test]
async fn a_named_model_that_is_not_ready_is_reported_with_the_providers_remedy() {
    let brain = FakeBrain::recorded().await;
    let missing = provider(&brain.url, "acme/gone")
        .readiness(Duration::from_secs(2))
        .await
        .expect_err("not installed");
    assert_eq!(missing.class(), ErrorClass::ModelNotInstalled);
    assert!(
        missing.to_string().contains("brain pull acme/gone"),
        "{missing}"
    );

    let broken = provider(&brain.url, "acme/broken")
        .readiness(Duration::from_secs(2))
        .await
        .expect_err("incompatible");
    assert_eq!(broken.class(), ErrorClass::FailedToLoad);
    assert!(broken.to_string().contains("option head"), "{broken}");

    let unknown = provider(&brain.url, "acme/nope")
        .readiness(Duration::from_secs(2))
        .await
        .expect_err("unknown");
    assert_eq!(unknown.class(), ErrorClass::ModelNotInstalled);
    assert!(
        !unknown.to_string().contains("brain pull"),
        "no invented catalog id: {unknown}"
    );
}

#[tokio::test]
async fn an_unvalidated_model_is_refused_when_validation_is_required() {
    let brain = FakeBrain::recorded().await;
    let strict = BrainProvider::new(BrainSettings {
        model: "acme/triage".into(),
        minimum_quality: MinimumQuality::Validated,
        ..BrainSettings::new(&brain.url)
    })
    .unwrap();
    let err = strict
        .decide(&recorded_request())
        .await
        .expect_err("refused");
    assert_eq!(err.class(), ErrorClass::UnsupportedCapability);
    assert!(err.to_string().contains("validat"), "{err}");
    assert_eq!(brain.calls_of("decide"), 0);

    let lenient = provider(&brain.url, "acme/triage");
    lenient.decide(&recorded_request()).await.unwrap();
}

#[tokio::test]
async fn an_identical_decision_of_a_versioned_model_is_asked_once() {
    let brain = FakeBrain::recorded().await;
    let p = provider(&brain.url, LAYA);
    let first = p.decide(&recorded_request()).await.unwrap();
    let second = p.decide(&recorded_request()).await.unwrap();
    assert!(
        !first.cached && second.cached,
        "a hit says it ran no inference"
    );
    assert_eq!(
        first,
        DecisionResult {
            cached: false,
            ..second
        }
    );
    assert_eq!(brain.calls_of("decide"), 1);

    let changed = DecisionRequest {
        state: "something else".into(),
        ..recorded_request()
    };
    p.decide(&changed).await.unwrap();
    assert_eq!(brain.calls_of("decide"), 2);
}

#[tokio::test]
async fn a_model_with_no_checkpoint_identity_is_never_served_from_the_cache() {
    let brain = FakeBrain::recorded().await;
    let p = provider(&brain.url, "acme/triage");
    p.decide(&recorded_request()).await.unwrap();
    p.decide(&recorded_request()).await.unwrap();
    assert_eq!(brain.calls_of("decide"), 2);
}

#[tokio::test]
async fn concurrent_decisions_never_exceed_the_configured_bound() {
    let brain = FakeBrain::start(|r| {
        let reply = support::recorded_reply(r);
        if r.action() == "decide" {
            reply.delayed(Duration::from_millis(120))
        } else {
            reply
        }
    })
    .await;
    let p = Arc::new(
        BrainProvider::new(BrainSettings {
            model: LAYA.into(),
            max_concurrent_requests: 2,
            ..BrainSettings::new(&brain.url)
        })
        .unwrap(),
    );
    let mut tasks = Vec::new();
    for n in 0..6 {
        let p = p.clone();
        tasks.push(tokio::spawn(async move {
            let request = DecisionRequest {
                state: format!("state {n}"),
                deadline: Duration::from_secs(5),
                ..recorded_request()
            };
            p.decide(&request).await
        }));
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    assert!(brain.max_in_flight() <= 2, "{}", brain.max_in_flight());
    assert!(
        brain.max_in_flight() >= 2,
        "the bound is used, not just respected"
    );
}

#[tokio::test]
async fn a_request_outside_the_contract_is_refused_before_anything_is_sent() {
    let brain = FakeBrain::recorded().await;
    let request = DecisionRequest {
        questions: Vec::<Question>::new(),
        ..recorded_request()
    };
    let err = provider(&brain.url, LAYA)
        .decide(&request)
        .await
        .expect_err("refused");
    assert_eq!(err.class(), ErrorClass::InvalidRequest);
    assert!(brain.received().is_empty());
}
