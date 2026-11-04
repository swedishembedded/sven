// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Spec: independent questions about one state travel in one call, and an
//! identical versioned decision is never asked twice.

use std::num::NonZeroUsize;
use std::time::Duration;

use sven_decision::{
    decide_batched, Answer, Choice, ChoiceOption, DecisionCache, DecisionError, DecisionRequest,
    DecisionResult, ErrorClass, MockDecisionProvider, ModelIdentity, Question, QuestionKind,
};

fn choice(id: &str, options: &[&str]) -> Question {
    Question::Choice(Choice {
        id: id.into(),
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

fn request(state: &str, questions: Vec<Question>, ms: u64) -> DecisionRequest {
    DecisionRequest {
        state: state.into(),
        questions,
        deadline: Duration::from_millis(ms),
    }
}

fn answer(winner: &str, other: &str) -> Answer {
    Answer::choice(&[(winner, 0.9), (other, 0.1)]).unwrap()
}

#[tokio::test]
async fn questions_about_the_same_state_travel_in_one_call_and_come_back_to_their_askers() {
    let mock = MockDecisionProvider::new("m");
    // Both askers use the question id "q": the merged call must keep them apart.
    mock.push_result(DecisionResult::for_answers(
        "m",
        vec![
            ("0:q", QuestionKind::Choice, answer("a", "b")),
            ("1:q", QuestionKind::Choice, answer("d", "c")),
        ],
    ));
    let requests = [
        request("same state", vec![choice("q", &["a", "b"])], 900),
        request("same state", vec![choice("q", &["c", "d"])], 300),
    ];
    let batched = decide_batched(&mock, &requests).await;

    assert_eq!(batched.calls, 1);
    let sent = mock.requests();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].questions.len(), 2);
    assert_eq!(
        sent[0].deadline,
        Duration::from_millis(300),
        "the tightest deadline wins"
    );

    let tops: Vec<String> = batched
        .results
        .iter()
        .map(|r| {
            let result = r.as_ref().unwrap();
            let Answer::Ranked(d) = &result.answer("q").unwrap().answer else {
                panic!("ranked")
            };
            d.top().id.clone()
        })
        .collect();
    assert_eq!(tops, ["a", "d"]);
}

#[tokio::test]
async fn different_states_are_different_calls_and_a_failure_reaches_every_member_of_its_batch() {
    let mock = MockDecisionProvider::new("m");
    mock.push_error(DecisionError::Timeout {
        deadline: Duration::from_millis(5),
    });
    mock.push_result(DecisionResult::for_answers(
        "m",
        vec![("2:q", QuestionKind::Choice, answer("a", "b"))],
    ));
    let requests = [
        request("one", vec![choice("q", &["a", "b"])], 100),
        request("one", vec![choice("r", &["a", "b"])], 100),
        request("two", vec![choice("q", &["a", "b"])], 100),
    ];
    let batched = decide_batched(&mock, &requests).await;
    assert_eq!(batched.calls, 2);
    assert_eq!(
        batched.results[0].as_ref().unwrap_err().class(),
        ErrorClass::Timeout
    );
    assert_eq!(
        batched.results[1].as_ref().unwrap_err().class(),
        ErrorClass::Timeout
    );
    assert!(batched.results[2].is_ok());
}

#[tokio::test]
async fn an_invalid_request_is_refused_alone_and_never_sent() {
    let mock = MockDecisionProvider::new("m");
    mock.push_result(DecisionResult::for_answers(
        "m",
        vec![("0:q", QuestionKind::Choice, answer("a", "b"))],
    ));
    let requests = [
        request("s", vec![choice("q", &["a", "b"])], 100),
        request("s", vec![], 100),
    ];
    let batched = decide_batched(&mock, &requests).await;
    assert!(batched.results[0].is_ok());
    assert_eq!(
        batched.results[1].as_ref().unwrap_err().class(),
        ErrorClass::InvalidRequest
    );
    assert_eq!(mock.requests()[0].questions.len(), 1);
}

fn identity(id: &str, checkpoint: Option<&str>) -> ModelIdentity {
    ModelIdentity {
        id: id.into(),
        checkpoint: checkpoint.map(Into::into),
    }
}

fn result(model: &str) -> DecisionResult {
    DecisionResult::for_answers(model, vec![("q", QuestionKind::Choice, answer("a", "b"))])
}

#[test]
fn an_identical_versioned_decision_is_a_hit_and_the_deadline_is_not_part_of_it() {
    let cache = DecisionCache::new(NonZeroUsize::new(8).unwrap());
    let model = identity("m", Some("sha256:aa"));
    let asked = request("s", vec![choice("q", &["a", "b"])], 100);
    let key = DecisionCache::key(&model, &asked).unwrap();
    assert!(cache.get(&key).is_none());
    cache.put(key.clone(), result("m"));

    let later = request("s", vec![choice("q", &["a", "b"])], 9_000);
    let again = DecisionCache::key(&model, &later).unwrap();
    assert_eq!(cache.get(&again), Some(result("m")));
}

#[test]
fn a_different_checkpoint_option_order_or_state_is_a_miss() {
    let cache = DecisionCache::new(NonZeroUsize::new(8).unwrap());
    let asked = request("s", vec![choice("q", &["a", "b"])], 100);
    let key = DecisionCache::key(&identity("m", Some("sha256:aa")), &asked).unwrap();
    cache.put(key, result("m"));

    for (model, req) in [
        (identity("m", Some("sha256:bb")), asked.clone()),
        (identity("other", Some("sha256:aa")), asked.clone()),
        (
            identity("m", Some("sha256:aa")),
            request("s", vec![choice("q", &["b", "a"])], 100),
        ),
        (
            identity("m", Some("sha256:aa")),
            request("t", vec![choice("q", &["a", "b"])], 100),
        ),
    ] {
        let key = DecisionCache::key(&model, &req).unwrap();
        assert!(cache.get(&key).is_none());
    }
}

#[test]
fn a_model_without_a_checkpoint_identity_is_never_cached() {
    let asked = request("s", vec![choice("q", &["a", "b"])], 100);
    assert!(DecisionCache::key(&identity("m", None), &asked).is_none());
}

#[test]
fn the_cache_never_holds_more_than_its_bound() {
    let cache = DecisionCache::new(NonZeroUsize::new(2).unwrap());
    let model = identity("m", Some("sha256:aa"));
    for state in ["1", "2", "3"] {
        let key =
            DecisionCache::key(&model, &request(state, vec![choice("q", &["a", "b"])], 1)).unwrap();
        cache.put(key, result("m"));
    }
    assert_eq!(cache.len(), 2);
    let oldest =
        DecisionCache::key(&model, &request("1", vec![choice("q", &["a", "b"])], 1)).unwrap();
    assert!(cache.get(&oldest).is_none());
}
