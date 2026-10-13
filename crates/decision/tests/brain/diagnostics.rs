// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Diagnostics: one message per session naming the exact remedy, the
//! required-mode refusal, and the doctor's report.

use std::time::Duration;

use sven_decision::config::DecisionConfig;
use sven_decision::doctor::{render_models, run_doctor, Status};
use sven_decision::{unavailable_text, DecisionError, DecisionRuntime, ErrorClass};

use crate::support::{self, closed, FakeBrain};

fn config(url: &str, yaml_overrides: &str) -> DecisionConfig {
    let mut c: DecisionConfig = serde_yaml::from_str(&format!(
        "enabled: true\nbrain: {{url: '{url}'}}\n{yaml_overrides}"
    ))
    .unwrap();
    c.timeout_ms = 500;
    c
}

#[test]
fn each_unavailable_state_names_its_own_remedy() {
    let c = DecisionConfig::default();
    let cases = [
        (
            DecisionError::Unreachable("refused".into()),
            vec![
                "http://127.0.0.1:8788",
                "decision.brain.url",
                "decision.enabled: false",
            ],
        ),
        (
            DecisionError::ModelNotInstalled {
                model: "acme/gone".into(),
                detail: "run `brain pull acme/gone`".into(),
            },
            vec!["acme/gone", "brain pull acme/gone"],
        ),
        (
            DecisionError::ModelNotInstalled {
                model: "auto".into(),
                detail: "no decision model is installed".into(),
            },
            vec!["sven decisions models", "decision.model"],
        ),
        (
            DecisionError::FailedToLoad {
                model: "acme/broken".into(),
                detail: "no option head".into(),
            },
            vec!["acme/broken", "no option head", "sven decisions models"],
        ),
        (
            DecisionError::UnsupportedCapability("no validation".into()),
            vec!["no validation", "sven doctor decisions"],
        ),
        (
            DecisionError::Timeout {
                deadline: Duration::from_millis(1),
            },
            vec!["500 ms", "decision.timeout_ms"],
        ),
    ];
    for (error, needles) in cases {
        let text = unavailable_text(&c, &error);
        for needle in needles {
            assert!(text.contains(needle), "{needle:?} missing from {text}");
        }
    }
    let invented = unavailable_text(
        &c,
        &DecisionError::ModelNotInstalled {
            model: "auto".into(),
            detail: "none".into(),
        },
    );
    assert!(
        !invented.contains("brain pull"),
        "no catalog id the provider did not report"
    );
}

#[test]
fn a_session_gives_one_diagnostic_however_many_decisions_fail() {
    let runtime = DecisionRuntime::new(config("http://127.0.0.1:1", "")).unwrap();
    let first = runtime.diagnose(&DecisionError::Unreachable("x".into()));
    assert!(first.unwrap().starts_with("Decisions unavailable"));
    assert!(runtime
        .diagnose(&DecisionError::Unreachable("x".into()))
        .is_none());
    assert!(runtime
        .diagnose(&DecisionError::Timeout {
            deadline: Duration::ZERO
        })
        .is_none());
}

#[test]
fn the_diagnostic_can_be_silenced() {
    let runtime =
        DecisionRuntime::new(config("http://127.0.0.1:1", "warn_if_unavailable: false")).unwrap();
    assert!(runtime
        .diagnose(&DecisionError::Unreachable("x".into()))
        .is_none());
}

#[tokio::test]
async fn decisions_that_are_off_never_contact_anything() {
    let brain = FakeBrain::recorded().await;
    let off = DecisionRuntime::new(config(&brain.url, "mode: off")).unwrap();
    let disabled = DecisionRuntime::new(DecisionConfig {
        enabled: false,
        ..config(&brain.url, "mode: required")
    })
    .unwrap();
    for runtime in [&off, &disabled] {
        assert!(runtime.provider().is_none());
        assert!(!runtime.feature_enabled(sven_decision::config::Feature::ToolRouting));
        runtime.require_ready().await.unwrap();
    }
    assert!(brain.received().is_empty());
}

#[tokio::test]
async fn required_mode_refuses_to_start_without_a_suitable_model() {
    let unreachable = DecisionRuntime::new(config(&closed().await, "mode: required")).unwrap();
    let err = unreachable.require_ready().await.expect_err("refused");
    assert!(
        err.contains("required") && err.contains("decision.brain.url"),
        "{err}"
    );

    let brain = FakeBrain::recorded().await;
    let missing =
        DecisionRuntime::new(config(&brain.url, "mode: required\nmodel: acme/gone")).unwrap();
    let err = missing.require_ready().await.expect_err("refused");
    assert!(err.contains("brain pull acme/gone"), "{err}");

    let ready = DecisionRuntime::new(config(&brain.url, "mode: required")).unwrap();
    ready.require_ready().await.unwrap();
    assert_eq!(brain.calls_of("decide"), 0, "readiness loads nothing");
}

#[tokio::test]
async fn auto_mode_does_not_probe_at_startup() {
    let brain = FakeBrain::recorded().await;
    let auto = DecisionRuntime::new(config(&brain.url, "mode: auto")).unwrap();
    auto.require_ready().await.unwrap();
    assert!(brain.received().is_empty());
}

#[tokio::test]
async fn the_doctor_reports_model_limits_validation_latency_and_inactive_features() {
    let brain = FakeBrain::start(support::recorded_with_echo).await;
    let mut c = config(&brain.url, "policy: {max_decision_input_tokens: 400}");
    c.features.cache_routing = false;
    let report = run_doctor(&c).await;

    assert!(report.is_ready(), "{report:?}");
    let model = report.model.as_ref().unwrap();
    assert_eq!(model.id, "convaiinnovations/laya");
    assert!(model.validated);
    assert_eq!(model.input_limit_tokens, Some(512));
    assert_eq!(model.input_budget_tokens, 400);
    assert_eq!(model.question_types, ["choice", "score", "probability"]);
    assert!(report.latency.is_some());
    assert_eq!(brain.calls_of("decide"), 3);

    let feature = |name: &str| report.features.iter().find(|f| f.name == name).unwrap();
    assert!(feature("tool_routing").active);
    assert!(!feature("cache_routing").active);
    assert!(feature("cache_routing")
        .reason
        .contains("decision.features.cache_routing"));
    assert!(!feature("model_routing").active);
    assert!(feature("context_selection")
        .reason
        .contains("no decision point"));

    let text = report.render();
    for needle in [
        "convaiinnovations/laya",
        "validated on ticket-routing",
        "400",
        "latency",
        "tool_routing",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
}

#[tokio::test]
async fn the_doctor_probes_even_when_decisions_are_off_and_says_so() {
    let brain = FakeBrain::start(support::recorded_with_echo).await;
    let c = DecisionConfig {
        enabled: false,
        ..config(&brain.url, "")
    };
    let report = run_doctor(&c).await;
    assert_eq!(report.mode, "off");
    assert!(report.is_ready());
    assert!(!report.features.iter().any(|f| f.active));
}

#[tokio::test]
async fn the_doctor_names_why_decisions_are_unavailable() {
    let report = run_doctor(&config(&closed().await, "")).await;
    let Status::Unavailable { class, message } = &report.status else {
        panic!("unavailable")
    };
    assert_eq!(*class, ErrorClass::Unreachable);
    assert!(message.contains("decision.brain.url"));
    assert!(report.model.is_none() && report.latency.is_none());

    let brain = FakeBrain::recorded().await;
    let report = run_doctor(&config(&brain.url, "model: acme/gone")).await;
    let Status::Unavailable { message, .. } = &report.status else {
        panic!("unavailable")
    };
    assert!(message.contains("brain pull acme/gone"), "{message}");
}

#[tokio::test]
async fn the_models_listing_shows_state_validation_and_the_remedy() {
    let brain = FakeBrain::recorded().await;
    let c = config(&brain.url, "model: acme/gone");
    let provider = sven_decision::brain::BrainProvider::new(c.brain_settings()).unwrap();
    let models = provider.models(Duration::from_secs(2)).await.unwrap();
    let text = render_models(&c, &models);
    for needle in [
        "convaiinnovations/laya",
        "validated on ticket-routing",
        "512 tokens",
        "acme/gone (configured)",
        "run `brain pull acme/gone`",
        "incompatible",
    ] {
        assert!(text.contains(needle), "{needle}\n{text}");
    }
}

#[test]
fn a_section_that_is_off_is_not_checked() {
    let broken = DecisionConfig {
        timeout_ms: 0,
        ..DecisionConfig::default()
    };
    assert!(DecisionRuntime::new(broken.clone()).is_ok());
    let enabled = DecisionConfig {
        enabled: true,
        ..broken
    };
    assert!(DecisionRuntime::new(enabled).is_err());
}
