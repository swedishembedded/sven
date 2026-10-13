// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The recorded wire, pinned: what the adapter sends and how it reads what
//! comes back.

use std::time::Duration;

use serde_json::Value;
use sven_decision::brain::{BrainProvider, BrainSettings};
use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionProvider, DecisionRequest, Probability, Question,
    QuestionKind, Score,
};

use crate::support::{self, FakeBrain, Reply};

pub fn provider(url: &str, model: &str) -> BrainProvider {
    BrainProvider::new(BrainSettings {
        model: model.into(),
        ..BrainSettings::new(url)
    })
    .unwrap()
}

pub fn recorded_request() -> DecisionRequest {
    DecisionRequest {
        state: "The user asks to rename a function across the repository.".into(),
        questions: vec![
            Question::Choice(Choice {
                id: "tool".into(),
                instruction: "Which tool fits the request?".into(),
                options: vec![
                    ChoiceOption {
                        id: "edit_file".into(),
                        description: Some("Edit a file in place.".into()),
                    },
                    ChoiceOption {
                        id: "grep".into(),
                        description: None,
                    },
                ],
            }),
            Question::Probability(Probability {
                id: "urgent".into(),
                proposition: "is this urgent".into(),
            }),
            Question::Score(Score {
                id: "anger".into(),
                instruction: "how upset".into(),
                levels: vec!["calm".into(), "annoyed".into(), "furious".into()],
            }),
        ],
        deadline: Duration::from_millis(500),
    }
}

#[tokio::test]
async fn the_request_on_the_wire_is_the_recorded_one() {
    let brain = FakeBrain::recorded().await;
    provider(&brain.url, "convaiinnovations/laya")
        .decide(&recorded_request())
        .await
        .unwrap();

    let sent = brain
        .received()
        .into_iter()
        .find(|r| r.action() == "decide")
        .expect("a decide call");
    assert_eq!(sent.path, "/v1/run");
    // On the wire `questions` is the document as a string; the fixture
    // spells it as the document.
    let mut actual = sent.body.clone();
    let questions: Value =
        serde_json::from_str(actual["params"]["questions"].as_str().unwrap()).unwrap();
    actual["params"]["questions"] = questions;
    let expected: Value = serde_json::from_str(support::REQUEST).unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn the_recorded_response_keeps_the_full_distribution_of_every_question() {
    let brain = FakeBrain::recorded().await;
    let result = provider(&brain.url, "convaiinnovations/laya")
        .decide(&recorded_request())
        .await
        .unwrap();

    assert_eq!(result.model.id, "convaiinnovations/laya");
    assert_eq!(
        result.model.checkpoint.as_deref(),
        Some("sha256:1111111111111111111111111111111111111111111111111111111111111111")
    );
    assert_eq!(result.latency, Duration::from_micros(41_200));
    assert_eq!(result.max_input_tokens, Some(512));
    assert!(!result.state_truncated);
    assert!(result.unsupported_kinds.is_empty());
    assert_eq!(result.answers.len(), 3);

    let Answer::Ranked(tool) = &result.answer("tool").unwrap().answer else {
        panic!("ranked")
    };
    assert_eq!(tool.ranked.len(), 2);
    assert_eq!(tool.top().id, "edit_file");
    assert_eq!(tool.confidence, Some(0.91));

    let urgent = result.answer("urgent").unwrap();
    assert_eq!(urgent.kind, QuestionKind::Probability);
    let Answer::Ranked(urgent) = &urgent.answer else {
        panic!("ranked")
    };
    assert_eq!(urgent.probability_of("true"), Some(0.22));
    assert_eq!(urgent.top().id, "false");

    let Answer::Ranked(anger) = &result.answer("anger").unwrap().answer else {
        panic!("ranked")
    };
    let order: Vec<usize> = anger.ranked.iter().map(|c| c.index).collect();
    assert_eq!(order, [1, 2, 0]);
}

#[tokio::test]
async fn truncation_and_unsupported_question_kinds_are_reported() {
    let brain = FakeBrain::start(|r| {
        if r.action() == "decide" {
            Reply::ok(support::DECIDE_FLAGS)
        } else {
            support::recorded_reply(r)
        }
    })
    .await;
    let result = provider(&brain.url, "convaiinnovations/laya")
        .decide(&recorded_request())
        .await
        .unwrap();
    assert!(result.state_truncated);
    assert_eq!(result.unsupported_kinds, [QuestionKind::Probability]);
    assert!(matches!(
        result.answer("urgent").unwrap().answer,
        Answer::Unsupported { .. }
    ));
    let Answer::Ranked(tool) = &result.answer("tool").unwrap().answer else {
        panic!("ranked")
    };
    assert!(tool.state_truncated);
}
