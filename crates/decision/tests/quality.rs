// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the quality of a decision point is read off its traces. Without
//! labels it says how often the point answered, abstained and why, and whether
//! the same inputs got the same answer; with labels it says how often an
//! accepted answer was right, how much of the labelled work was answered, and
//! whether stated confidence matches how often the answers were right.
//!
//! Swedish Embedded AB implements solutions for measuring small decision models
//! inside agent runtimes for its clients. If your team needs expertise in
//! calibrating and gating model advice then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;

use sven_decision::quality::{point_quality, Labels};
use sven_vocab::DecisionTraceRecord;

fn accept(policy: &str, digest: &str, selected: &str, probability: f32) -> DecisionTraceRecord {
    DecisionTraceRecord {
        policy: policy.into(),
        inputs_digest: digest.into(),
        verdict: "accept".into(),
        selected: Some(selected.into()),
        probability: Some(probability),
        applied: true,
        ..DecisionTraceRecord::default()
    }
}

fn abstain(policy: &str, digest: &str, reason: &str) -> DecisionTraceRecord {
    DecisionTraceRecord {
        policy: policy.into(),
        inputs_digest: digest.into(),
        verdict: "abstain".into(),
        abstain_reason: Some(reason.into()),
        ..DecisionTraceRecord::default()
    }
}

#[test]
fn without_labels_a_point_reports_its_rate_of_answers_its_abstentions_and_its_stability() {
    let traces = vec![
        accept("routing", "d1", "a", 0.9),
        accept("routing", "d2", "b", 0.8),
        abstain("routing", "d3", "low_confidence"),
        abstain("routing", "d4", "provider_failed:timeout"),
        abstain("routing", "d5", "low_confidence"),
        // The same inputs asked twice with two different answers.
        accept("routing", "d1", "b", 0.7),
        accept("ranking", "e1", "x", 0.99),
    ];
    let quality = point_quality(&traces, &Labels::default());
    let routing = &quality["routing"];
    assert_eq!(
        (routing.decisions, routing.accepted, routing.abstained),
        (6, 3, 3)
    );
    assert_eq!(routing.abstain_by_reason["low_confidence"], 2);
    assert_eq!(routing.abstain_by_reason["provider_failed:timeout"], 1);
    assert_eq!(routing.unstable_inputs, 1, "d1 got two different answers");
    assert_eq!(routing.labelled, 0);
    assert!(routing.accuracy_on_accepted.is_none() && routing.coverage.is_none());
    assert_eq!(quality["ranking"].decisions, 1);
}

#[test]
fn with_labels_it_reports_accuracy_on_what_it_answered_and_coverage_of_what_was_asked() {
    let traces = vec![
        accept("routing", "d1", "a", 0.9),
        accept("routing", "d2", "b", 0.8),
        accept("routing", "d3", "c", 0.6),
        abstain("routing", "d4", "near_tie"),
        accept("routing", "unlabelled", "a", 0.5),
    ];
    let labels = Labels::new(BTreeMap::from([
        ("d1".to_string(), "a".to_string()),
        ("d2".to_string(), "x".to_string()),
        ("d3".to_string(), "c".to_string()),
        ("d4".to_string(), "a".to_string()),
    ]));
    let routing = &point_quality(&traces, &labels)["routing"];
    assert_eq!(routing.labelled, 4, "the unlabelled decision is not scored");
    assert_eq!((routing.correct, routing.wrong), (2, 1));
    assert!((routing.accuracy_on_accepted.unwrap() - 2.0 / 3.0).abs() < 1e-9);
    assert!((routing.coverage.unwrap() - 3.0 / 4.0).abs() < 1e-9);
}

#[test]
fn calibration_compares_stated_probability_with_how_often_the_answers_were_right() {
    // Ten answers claiming 0.9, nine of them right; ten claiming 0.5, five right.
    let mut traces = Vec::new();
    let mut labels = BTreeMap::new();
    for i in 0..10 {
        let digest = format!("hi{i}");
        traces.push(accept("p", &digest, "a", 0.9));
        labels.insert(digest, if i < 9 { "a" } else { "z" }.to_string());
        let digest = format!("mid{i}");
        traces.push(accept("p", &digest, "a", 0.5));
        labels.insert(digest, if i < 5 { "a" } else { "z" }.to_string());
    }
    let p = &point_quality(&traces, &Labels::new(labels))["p"];
    let bin = |lo: f64| {
        p.calibration
            .iter()
            .find(|b| (b.lower - lo).abs() < 1e-9)
            .unwrap()
    };
    assert_eq!((bin(0.9).n, bin(0.9).accuracy), (10, 0.9));
    assert_eq!((bin(0.5).n, bin(0.5).accuracy), (10, 0.5));
    // Both bins are right on average to within a bin's width: a small error.
    assert!(p.expected_calibration_error.unwrap() < 0.06);
}
