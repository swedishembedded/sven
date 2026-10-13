// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: the decision contract. A request carries typed questions about one
//! state; a result keeps the whole ranked distribution of every question; a
//! provider fails in one of a small set of distinct, named ways.

use std::time::Duration;

use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionError, DecisionProvider, DecisionRequest, DecisionResult,
    ErrorClass, MockDecisionProvider, Probability, Question, QuestionKind,
};

fn choice(id: &str, options: &[&str]) -> Question {
    Question::Choice(Choice {
        id: id.into(),
        instruction: "which one".into(),
        options: options
            .iter()
            .map(|o| ChoiceOption {
                id: (*o).into(),
                description: None,
            })
            .collect(),
    })
}

fn request(questions: Vec<Question>) -> DecisionRequest {
    DecisionRequest {
        state: "the user wants to edit a file".into(),
        questions,
        deadline: Duration::from_millis(500),
    }
}

#[test]
fn a_well_formed_request_validates() {
    let r = request(vec![
        choice("tool", &["edit_file", "shell"]),
        Question::Probability(Probability {
            id: "urgent".into(),
            proposition: "is this urgent".into(),
        }),
    ]);
    assert_eq!(r.validate(), Ok(()));
}

#[test]
fn a_request_outside_the_published_limits_is_refused_before_it_is_sent() {
    let too_many_options: Vec<String> = (0..256).map(|i| format!("o{i}")).collect();
    let refs: Vec<&str> = too_many_options.iter().map(String::as_str).collect();
    let cases = [
        request(vec![]),
        request(vec![choice("q", &[])]),
        request(vec![choice("q", &refs)]),
        request(vec![choice("q", &["a", "a"])]),
        request(vec![choice("q", &["a"]), choice("q", &["b"])]),
        request(vec![choice("", &["a"])]),
        DecisionRequest {
            deadline: Duration::ZERO,
            ..request(vec![choice("q", &["a"])])
        },
    ];
    for case in cases {
        let err = case.validate().expect_err("must be refused");
        assert_eq!(err.class(), ErrorClass::InvalidRequest, "{err}");
    }
}

#[test]
fn a_distribution_is_ranked_best_first_and_keeps_every_candidate() {
    let answer = Answer::choice(&[("a", 0.2), ("b", 0.5), ("c", 0.3)]).unwrap();
    let Answer::Ranked(d) = answer else {
        panic!("a ranked answer")
    };
    let order: Vec<(&str, usize)> = d.ranked.iter().map(|c| (c.id.as_str(), c.index)).collect();
    assert_eq!(order, [("b", 1), ("c", 2), ("a", 0)]);
    assert_eq!(d.top().id, "b");
    assert!((d.margin() - 0.2).abs() < 1e-6);
}

#[test]
fn a_distribution_that_is_not_a_distribution_is_an_invalid_response() {
    for bad in [
        vec![],
        vec![("a", f32::NAN)],
        vec![("a", 1.5)],
        vec![("a", -0.1)],
    ] {
        let err = Answer::choice(&bad).expect_err("must be refused");
        assert_eq!(err.class(), ErrorClass::InvalidResponse);
    }
}

#[tokio::test]
async fn the_mock_answers_in_script_order_and_records_what_it_was_asked() {
    let mock = MockDecisionProvider::new("mock/model");
    mock.push_result(DecisionResult::for_answers(
        "mock/model",
        vec![(
            "tool",
            QuestionKind::Choice,
            Answer::choice(&[("shell", 0.9), ("edit_file", 0.1)]).unwrap(),
        )],
    ));
    mock.push_error(DecisionError::Timeout {
        deadline: Duration::from_millis(500),
    });

    let asked = request(vec![choice("tool", &["edit_file", "shell"])]);
    let first = mock.decide(&asked).await.unwrap();
    assert_eq!(first.model.id, "mock/model");
    let Answer::Ranked(d) = &first.answer("tool").unwrap().answer else {
        panic!("ranked")
    };
    assert_eq!(d.top().id, "shell");

    let second = mock.decide(&asked).await.expect_err("injected failure");
    assert_eq!(second.class(), ErrorClass::Timeout);

    let exhausted = mock.decide(&asked).await.expect_err("script exhausted");
    assert_eq!(exhausted.class(), ErrorClass::Unreachable);
    assert_eq!(mock.requests().len(), 3);
    assert_eq!(mock.requests()[0], asked);
}

#[test]
fn every_failure_class_has_its_own_stable_name() {
    let errors = [
        DecisionError::Unreachable("refused".into()),
        DecisionError::ModelNotInstalled {
            model: "m".into(),
            detail: "gone".into(),
        },
        DecisionError::FailedToLoad {
            model: "m".into(),
            detail: "bad".into(),
        },
        DecisionError::UnsupportedCapability("no".into()),
        DecisionError::Timeout {
            deadline: Duration::from_millis(1),
        },
        DecisionError::InvalidResponse("junk".into()),
        DecisionError::InvalidRequest("junk".into()),
    ];
    let mut names: Vec<&str> = errors.iter().map(|e| e.class().name()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), errors.len());
}
