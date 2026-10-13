// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: a decision point declares its own policy, and `evaluate` turns a
//! model's answer into an acceptance or an explicit abstention. There is no
//! global threshold, and an answer never grants anything.

use std::time::Duration;

use sven_decision::policy::{
    agree, estimate_input_tokens, evaluate, evaluate_error, AbstainReason, AllowedShape, Cost, DecisionPolicy, FallbackAction,
    Impact, PolicyDecl, Selected, Thresholds, Verdict,
};
use sven_decision::{
    Answer, Candidate, Choice, ChoiceOption, DecisionError, DecisionResult, Distribution,
    ErrorClass, Probability, Question, QuestionKind,
};

fn decl(min_confidence: f32, min_margin: f32) -> PolicyDecl {
    PolicyDecl {
        label: "tool_routing".into(),
        shape: AllowedShape {
            kind: QuestionKind::Choice,
            max_options: 8,
        },
        max_input_tokens: 400,
        false_positive: Cost::Low,
        false_negative: Cost::Low,
        thresholds: Thresholds {
            min_confidence,
            min_margin,
        },
        impact: Impact::Routine,
        fallback: FallbackAction::Legacy,
    }
}

fn policy(min_confidence: f32, min_margin: f32) -> DecisionPolicy {
    DecisionPolicy::new(decl(min_confidence, min_margin)).unwrap()
}

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

fn result_of(answer: Answer) -> DecisionResult {
    DecisionResult::for_answers("m", vec![("q", QuestionKind::Choice, answer)])
}

fn dist(p: &[(&str, f32)]) -> Answer {
    Answer::choice(p).unwrap()
}

fn abstained(v: Verdict) -> AbstainReason {
    match v {
        Verdict::Abstain(reason) => reason,
        Verdict::Accept(a) => panic!("expected an abstention, accepted {a:?}"),
    }
}

#[test]
fn a_decisive_answer_is_accepted_with_its_evidence() {
    let q = choice(&["a", "b", "c"]);
    let r = result_of(dist(&[("a", 0.1), ("b", 0.8), ("c", 0.1)]));
    let Verdict::Accept(accepted) = evaluate(&policy(0.6, 0.2), &q, &r) else {
        panic!("accepted")
    };
    let Selected::Candidate {
        id,
        index,
        probability,
        margin,
    } = accepted.selected
    else {
        panic!("a candidate")
    };
    assert_eq!((id.as_str(), index, probability), ("b", 1, 0.8));
    assert!((margin - 0.7).abs() < 1e-6);
    assert!(!accepted.advisory_only);
    assert_eq!(accepted.policy, "tool_routing");
}

#[test]
fn the_same_answer_passes_one_policy_and_fails_another() {
    let q = choice(&["a", "b"]);
    let r = result_of(dist(&[("a", 0.7), ("b", 0.3)]));
    assert!(matches!(
        evaluate(&policy(0.6, 0.2), &q, &r),
        Verdict::Accept(_)
    ));
    assert!(matches!(
        abstained(evaluate(&policy(0.9, 0.2), &q, &r)),
        AbstainReason::LowConfidence { .. }
    ));
}

#[test]
fn the_models_own_confidence_is_held_to_the_threshold_too() {
    let q = choice(&["a", "b"]);
    let mut d = Distribution::from_candidates(&[("a", 0.9), ("b", 0.1)]).unwrap();
    d.confidence = Some(0.3);
    let r = result_of(Answer::Ranked(d));
    assert!(matches!(
        abstained(evaluate(&policy(0.6, 0.2), &q, &r)),
        AbstainReason::LowConfidence { .. }
    ));
}

#[test]
fn near_tied_top_options_abstain() {
    let q = choice(&["a", "b", "c"]);
    let r = result_of(dist(&[("a", 0.42), ("b", 0.40), ("c", 0.18)]));
    assert!(matches!(
        abstained(evaluate(&policy(0.4, 0.1), &q, &r)),
        AbstainReason::NearTie { .. }
    ));
}

#[test]
fn an_option_the_caller_never_offered_abstains() {
    let q = choice(&["a", "b"]);
    let r = result_of(dist(&[("zzz", 0.9), ("b", 0.1)]));
    assert_eq!(
        abstained(evaluate(&policy(0.6, 0.2), &q, &r)),
        AbstainReason::UnsupportedCandidate { id: "zzz".into() }
    );
}

#[test]
fn a_truncated_state_abstains_whether_flagged_on_the_result_or_the_answer() {
    let q = choice(&["a", "b"]);
    let mut on_result = result_of(dist(&[("a", 0.9), ("b", 0.1)]));
    on_result.state_truncated = true;
    assert_eq!(
        abstained(evaluate(&policy(0.6, 0.2), &q, &on_result)),
        AbstainReason::Truncated
    );
    let mut d = Distribution::from_candidates(&[("a", 0.9), ("b", 0.1)]).unwrap();
    d.state_truncated = true;
    assert_eq!(
        abstained(evaluate(
            &policy(0.6, 0.2),
            &q,
            &result_of(Answer::Ranked(d))
        )),
        AbstainReason::Truncated
    );
}

#[test]
fn a_question_the_model_declined_or_skipped_abstains() {
    let q = choice(&["a", "b"]);
    let declined = result_of(Answer::Unsupported {
        reason: "no".into(),
    });
    assert_eq!(
        abstained(evaluate(&policy(0.6, 0.2), &q, &declined)),
        AbstainReason::UnsupportedQuestion
    );
    let empty = DecisionResult::for_answers("m", vec![]);
    assert_eq!(
        abstained(evaluate(&policy(0.6, 0.2), &q, &empty)),
        AbstainReason::MissingAnswer
    );
}

#[test]
fn a_question_outside_the_allowed_shape_abstains() {
    let r = result_of(dist(&[("a", 0.9), ("b", 0.1)]));
    let many: Vec<String> = (0..9).map(|i| format!("o{i}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    assert!(matches!(
        abstained(evaluate(&policy(0.6, 0.2), &choice(&refs), &r)),
        AbstainReason::ShapeNotAllowed { .. }
    ));
    let proposition = Question::Probability(Probability {
        id: "q".into(),
        proposition: "p".into(),
    });
    assert!(matches!(
        abstained(evaluate(&policy(0.6, 0.2), &proposition, &r)),
        AbstainReason::ShapeNotAllowed { .. }
    ));
}

#[test]
fn a_provider_failure_is_an_abstention_naming_the_class() {
    let reason = abstained(evaluate_error(&DecisionError::Timeout {
        deadline: Duration::from_millis(5),
    }));
    assert_eq!(
        reason,
        AbstainReason::ProviderFailed {
            class: ErrorClass::Timeout
        }
    );
}

#[test]
fn a_proposition_is_accepted_as_holding_or_not_holding() {
    let mut d = decl(0.7, 0.2);
    d.shape.kind = QuestionKind::Probability;
    let policy = DecisionPolicy::new(d).unwrap();
    let q = Question::Probability(Probability {
        id: "q".into(),
        proposition: "p".into(),
    });
    let ranked = |p_true: f32| {
        let answer = Answer::Ranked(
            Distribution::ranked(vec![
                Candidate {
                    id: "false".into(),
                    index: 0,
                    probability: 1.0 - p_true,
                },
                Candidate {
                    id: "true".into(),
                    index: 1,
                    probability: p_true,
                },
            ])
            .unwrap(),
        );
        DecisionResult::for_answers("m", vec![("q", QuestionKind::Probability, answer)])
    };
    let Verdict::Accept(yes) = evaluate(&policy, &q, &ranked(0.9)) else {
        panic!("accepted")
    };
    assert!(matches!(
        yes.selected,
        Selected::Proposition { holds: true, .. }
    ));
    let Verdict::Accept(no) = evaluate(&policy, &q, &ranked(0.05)) else {
        panic!("accepted")
    };
    assert!(matches!(
        no.selected,
        Selected::Proposition { holds: false, .. }
    ));
    assert!(matches!(
        abstained(evaluate(&policy, &q, &ranked(0.55))),
        AbstainReason::LowConfidence { .. } | AbstainReason::NearTie { .. }
    ));
}

#[test]
fn a_policy_touching_security_or_irreversible_actions_is_advisory_only_by_construction() {
    let q = choice(&["a", "b"]);
    let r = result_of(dist(&[("a", 0.95), ("b", 0.05)]));
    for impact in [Impact::Security, Impact::Irreversible] {
        let mut d = decl(0.6, 0.2);
        d.impact = impact;
        let policy = DecisionPolicy::new(d).unwrap();
        assert!(policy.advisory_only());
        let Verdict::Accept(accepted) = evaluate(&policy, &q, &r) else {
            panic!("accepted")
        };
        assert!(accepted.advisory_only);
    }
}

#[test]
fn a_policy_with_thresholds_outside_the_unit_interval_is_refused() {
    for (confidence, margin) in [
        (0.0, 0.1),
        (1.5, 0.1),
        (0.5, -0.1),
        (0.5, 1.5),
        (f32::NAN, 0.1),
    ] {
        assert!(DecisionPolicy::new(decl(confidence, margin)).is_err());
    }
}

#[test]
fn a_verdict_carries_no_grant_of_any_kind() {
    let q = choice(&["a", "b"]);
    let accept = evaluate(
        &policy(0.6, 0.2),
        &q,
        &result_of(dist(&[("a", 0.9), ("b", 0.1)])),
    );
    let abstain = evaluate_error(&DecisionError::Unreachable("x".into()));
    let keys = |v: &Verdict| -> Vec<String> {
        let json = serde_json::to_value(v).unwrap();
        let mut keys: Vec<String> = json
            .as_object()
            .unwrap()
            .values()
            .flat_map(|inner| {
                inner
                    .as_object()
                    .map(|o| o.keys().cloned().collect::<Vec<_>>())
            })
            .flatten()
            .collect();
        keys.sort();
        keys
    };
    assert_eq!(keys(&accept), ["advisory_only", "policy", "selected"]);
    assert!(keys(&abstain).is_empty() || !keys(&abstain).iter().any(|k| k.contains("grant")));
}

#[test]
fn two_orders_agree_only_on_the_same_winner() {
    let q = choice(&["a", "b"]);
    let wins = |winner: &str, loser: &str, p: f32| {
        evaluate(
            &policy(0.6, 0.2),
            &q,
            &result_of(dist(&[(winner, p), (loser, 1.0 - p)])),
        )
    };
    let Verdict::Accept(both) = agree(&wins("a", "b", 0.95), &wins("a", "b", 0.8)) else {
        panic!("agreed")
    };
    let Selected::Candidate { margin, .. } = both.selected else {
        panic!("a candidate")
    };
    assert!((margin - 0.6).abs() < 1e-5, "the weaker evidence stands: {margin}");

    assert_eq!(
        abstained(agree(&wins("a", "b", 0.9), &wins("b", "a", 0.9))),
        AbstainReason::OrderSensitive {
            first: "a".into(),
            second: "b".into()
        }
    );
    let tie = wins("a", "b", 0.5);
    assert!(matches!(
        abstained(agree(&wins("a", "b", 0.9), &tie)),
        AbstainReason::LowConfidence { .. }
    ));
}

#[test]
fn the_token_estimate_is_pessimistic() {
    assert_eq!(estimate_input_tokens(""), 0);
    assert_eq!(estimate_input_tokens("abcd"), 2);
    assert_eq!(estimate_input_tokens("abc"), 1);
}
