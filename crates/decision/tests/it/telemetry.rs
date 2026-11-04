// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: every decision is accounted for. Inference cost is summed into the
//! ledger's `DecisionInference`, and every acceptance and abstention leaves a
//! trace record that names the policy and the inputs.

use std::time::Duration;

use sven_decision::config::DecisionConfig;
use sven_decision::policy::{
    evaluate, evaluate_error, AllowedShape, Cost, DecisionPolicy, FallbackAction, Impact,
    PolicyDecl, Thresholds, Verdict,
};
use sven_decision::telemetry::{inputs_digest, trace_record, CallRecord, DecisionTelemetry};
use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionError, DecisionRequest, DecisionResult, Question,
    QuestionKind,
};

fn choice(options: &[&str]) -> Question {
    Question::Choice(Choice {
        id: "q".into(),
        instruction: "which".into(),
        options: options
            .iter()
            .map(|o| ChoiceOption {
                id: (*o).into(),
                description: None,
            })
            .collect(),
    })
}

fn request(state: &str, options: &[&str], ms: u64) -> DecisionRequest {
    DecisionRequest {
        state: state.into(),
        questions: vec![choice(options)],
        deadline: Duration::from_millis(ms),
    }
}

fn policy(impact: Impact) -> DecisionPolicy {
    DecisionPolicy::new(PolicyDecl {
        label: "tool_routing".into(),
        shape: AllowedShape {
            kind: QuestionKind::Choice,
            max_options: 8,
        },
        max_input_tokens: 256,
        false_positive: Cost::Low,
        false_negative: Cost::Low,
        thresholds: Thresholds {
            min_confidence: 0.6,
            min_margin: 0.2,
        },
        impact,
        fallback: FallbackAction::Legacy,
    })
    .unwrap()
}

fn call(ms: u64, batch: usize) -> CallRecord {
    CallRecord {
        wall: Duration::from_millis(ms),
        model_id: Some("m".into()),
        batch_size: batch,
        cached: false,
    }
}

#[test]
fn nothing_asked_is_nothing_recorded() {
    let t = DecisionTelemetry::default();
    let drained = t.drain();
    assert!(drained.inference.is_none() && drained.traces.is_empty());
}

#[test]
fn inference_is_summed_per_drain_and_a_cache_hit_costs_nothing() {
    let t = DecisionTelemetry::default();
    t.record_call(&call(40, 3));
    t.record_call(&call(60, 2));
    t.record_call(&CallRecord {
        cached: true,
        ..call(1, 9)
    });
    let inference = t.drain().inference.unwrap();
    assert_eq!(inference.calls, 2);
    assert_eq!(inference.latency_ms, Some(100));
    assert_eq!(inference.model_id.as_deref(), Some("m"));
    assert_eq!(inference.batch_size, Some(3));
    assert_eq!(inference.load_time_ms, None, "the provider reports none");
    assert!(t.drain().inference.is_none(), "a drain empties the account");
}

#[test]
fn a_failed_call_is_still_a_call_that_took_time() {
    let t = DecisionTelemetry::default();
    t.record_call(&CallRecord {
        model_id: None,
        ..call(500, 1)
    });
    let inference = t.drain().inference.unwrap();
    assert_eq!((inference.calls, inference.latency_ms), (1, Some(500)));
    assert_eq!(inference.model_id, None);
}

#[test]
fn an_acceptance_is_traced_with_its_policy_inputs_and_whether_it_was_applied() {
    let q = choice(&["a", "b"]);
    let r = request("s", &["a", "b"], 100);
    let result = DecisionResult::for_answers(
        "m",
        vec![(
            "q",
            QuestionKind::Choice,
            Answer::choice(&[("a", 0.9), ("b", 0.1)]).unwrap(),
        )],
    );
    let verdict = evaluate(&policy(Impact::Security), &q, &result);
    let record = trace_record("tool_routing", &r, &verdict, Some("m"), false);
    assert_eq!(record.policy, "tool_routing");
    assert_eq!(record.verdict, "accept");
    assert_eq!(record.selected.as_deref(), Some("a"));
    assert_eq!(record.model_id.as_deref(), Some("m"));
    assert!(record.advisory_only && !record.applied);
    assert_eq!(record.inputs_digest, inputs_digest(&r));
}

#[test]
fn an_abstention_is_traced_with_its_reason() {
    let r = request("s", &["a", "b"], 100);
    let q = choice(&["a", "b"]);
    let tie = DecisionResult::for_answers(
        "m",
        vec![(
            "q",
            QuestionKind::Choice,
            Answer::choice(&[("a", 0.5), ("b", 0.5)]).unwrap(),
        )],
    );
    let near_tie = trace_record(
        "p",
        &r,
        &evaluate(&policy(Impact::Routine), &q, &tie),
        Some("m"),
        false,
    );
    assert_eq!(near_tie.verdict, "abstain");
    assert_eq!(near_tie.abstain_reason.as_deref(), Some("low_confidence"));
    assert_eq!(near_tie.selected, None);

    let failed: Verdict = evaluate_error(&DecisionError::Timeout {
        deadline: Duration::ZERO,
    });
    let record = trace_record("p", &r, &failed, None, false);
    assert_eq!(
        record.abstain_reason.as_deref(),
        Some("provider_failed:timeout")
    );
    assert_eq!(record.model_id, None);
}

#[test]
fn the_inputs_digest_identifies_what_was_asked_and_nothing_else() {
    let base = inputs_digest(&request("s", &["a", "b"], 100));
    assert_eq!(
        base,
        inputs_digest(&request("s", &["a", "b"], 9_000)),
        "the deadline is not an input"
    );
    assert_ne!(base, inputs_digest(&request("t", &["a", "b"], 100)));
    assert_ne!(
        base,
        inputs_digest(&request("s", &["b", "a"], 100)),
        "option order is an input"
    );
    assert_eq!(base.len(), 64);
}

#[test]
fn the_telemetry_travels_with_the_runtime_of_the_session() {
    let runtime = sven_decision::DecisionRuntime::new(DecisionConfig::default()).unwrap();
    runtime.telemetry().record_call(&call(5, 1));
    assert_eq!(runtime.telemetry().drain().inference.unwrap().calls, 1);
}
