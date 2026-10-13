// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the `decision` section of the configuration file.

use sven_config::{ConfigDocument, Schema};
use sven_decision::config::{
    DecisionConfig, DecisionMode, PolicyMode, DEFAULT_TIMEOUT_MS, FALLBACK_INPUT_TOKENS,
};
use sven_decision::MinimumQuality;

fn read(yaml: &str) -> DecisionConfig {
    let document = ConfigDocument::from_yaml(yaml).unwrap();
    document.decode::<Sectioned>().decision
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Sectioned {
    decision: DecisionConfig,
}

#[test]
fn nothing_is_contacted_unless_the_file_turns_decisions_on() {
    let c = DecisionConfig::default();
    assert!(!c.enabled);
    assert_eq!(c.effective_mode(), DecisionMode::Off);
    assert_eq!(c.mode, DecisionMode::Auto);
    assert_eq!(c.model, "auto");
    assert_eq!(c.timeout_ms, DEFAULT_TIMEOUT_MS);
    assert!(c.warn_if_unavailable);
    assert_eq!(c.brain.url, "http://127.0.0.1:8788");
    assert_eq!(c.policy.mode, PolicyMode::Apply);
    assert_eq!(c.policy.minimum_model_quality, MinimumQuality::Validated);
    assert_eq!(c.policy.max_decision_input_tokens, None);
    assert!(c.validate().is_ok());
}

#[test]
fn the_default_features_are_the_documented_ones() {
    let f = DecisionConfig::default().features;
    assert!(f.context_selection && f.tool_routing && f.skill_selection);
    assert!(f.delegation && f.cache_routing);
    assert!(!f.model_routing && !f.permission_advisory);
}

#[test]
fn a_file_overrides_only_what_it_names() {
    let c = read(
        "decision:\n  enabled: true\n  mode: required\n  timeout_ms: 900\n  \
         brain: {url: 'http://10.0.0.5:9000', api_key: k}\n  \
         features: {tool_routing: false}\n  \
         policy: {mode: shadow, max_decision_input_tokens: 300, minimum_model_quality: any}\n",
    );
    assert_eq!(c.effective_mode(), DecisionMode::Required);
    assert_eq!(c.timeout_ms, 900);
    assert_eq!(c.brain.url, "http://10.0.0.5:9000");
    assert_eq!(c.brain.api_key.as_deref(), Some("k"));
    assert!(!c.features.tool_routing && c.features.context_selection);
    assert_eq!(c.policy.mode, PolicyMode::Shadow);
    assert_eq!(c.policy.max_decision_input_tokens, Some(300));
    assert_eq!(c.policy.minimum_model_quality, MinimumQuality::Any);
    assert_eq!(c.model, "auto");
}

#[test]
fn off_wins_over_a_disabled_flag_and_a_disabled_flag_over_every_mode() {
    let mut c = read("decision: {enabled: true, mode: off}\n");
    assert_eq!(c.effective_mode(), DecisionMode::Off);
    c = read("decision: {enabled: false, mode: required}\n");
    assert_eq!(c.effective_mode(), DecisionMode::Off);
}

#[test]
fn the_input_budget_never_exceeds_what_the_served_model_reports() {
    let mut c = DecisionConfig::default();
    assert_eq!(c.input_budget_tokens(Some(512)), 512);
    assert_eq!(c.input_budget_tokens(None), FALLBACK_INPUT_TOKENS);
    c.policy.max_decision_input_tokens = Some(300);
    assert_eq!(c.input_budget_tokens(Some(512)), 300);
    assert_eq!(c.input_budget_tokens(Some(128)), 128);
    assert_eq!(c.input_budget_tokens(None), 300);
}

#[test]
fn values_outside_their_range_are_refused_naming_the_key() {
    for (yaml, key) in [
        ("decision: {timeout_ms: 0}", "timeout_ms"),
        ("decision: {timeout_ms: 700000}", "timeout_ms"),
        ("decision: {brain: {url: ''}}", "brain.url"),
        ("decision: {brain: {url: 'ftp://x'}}", "brain.url"),
        (
            "decision: {brain: {max_concurrent_requests: 0}}",
            "max_concurrent_requests",
        ),
        ("decision: {brain: {cache_entries: 0}}", "cache_entries"),
        (
            "decision: {policy: {max_decision_input_tokens: 0}}",
            "max_decision_input_tokens",
        ),
        ("decision: {model: ''}", "model"),
    ] {
        let err = read(yaml).validate().expect_err(yaml);
        assert!(err.contains(key), "{yaml}: {err}");
    }
}

#[test]
fn the_schema_knows_every_key_and_reports_a_typo() {
    let schema = Schema::fields([("decision", DecisionConfig::schema())]);
    let all = "decision:\n  enabled: true\n  mode: auto\n  provider: brain\n  model: auto\n  \
               timeout_ms: 500\n  warn_if_unavailable: true\n  \
               brain: {url: 'http://x', api_key: k, max_concurrent_requests: 2, cache_entries: 8}\n  \
               features: {context_selection: true, tool_routing: true, skill_selection: true, \
               delegation: true, model_routing: false, cache_routing: true, permission_advisory: false}\n  \
               policy: {mode: apply, max_decision_input_tokens: 400, minimum_model_quality: any}\n";
    let document = ConfigDocument::from_yaml(all).unwrap();
    assert!(document.unknown_keys(&schema).is_empty());
    let typo = ConfigDocument::from_yaml("decision: {features: {tool_routng: true}}\n").unwrap();
    let unknown = typo.unknown_keys(&schema);
    assert_eq!(unknown.len(), 1);
    assert!(
        unknown[0].contains("decision.features.tool_routng"),
        "{unknown:?}"
    );
}

#[test]
fn the_serialized_default_has_exactly_the_documented_keys() {
    let json = serde_json::to_value(DecisionConfig::default()).unwrap();
    let keys = |v: &serde_json::Value| -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    assert_eq!(
        keys(&json),
        [
            "brain",
            "enabled",
            "features",
            "mode",
            "model",
            "policy",
            "provider",
            "timeout_ms",
            "warn_if_unavailable"
        ]
    );
    assert_eq!(
        keys(&json["brain"]),
        ["cache_entries", "max_concurrent_requests", "url"]
    );
}
