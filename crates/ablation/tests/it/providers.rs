// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The decision providers of the benchmark: a rule-based heuristic, a seeded
//! random control and an oracle. Each answers like a model: a full valid
//! distribution per question.

use std::time::Duration;

use sven_ablation::providers::{HeuristicProvider, OracleLabel, OracleProvider, RandomProvider};
use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionProvider, DecisionRequest, Question, QuestionKind,
};

fn choice(task: &str, options: &[(&str, &str)]) -> DecisionRequest {
    DecisionRequest {
        state: format!("Task: {task}"),
        questions: vec![Question::Choice(Choice {
            id: "tool".into(),
            instruction: "Which option fits the task best?".into(),
            options: options
                .iter()
                .map(|(id, d)| ChoiceOption {
                    id: (*id).into(),
                    description: Some((*d).into()),
                })
                .collect(),
        })],
        deadline: Duration::from_secs(1),
    }
}

const OPTIONS: [(&str, &str); 4] = [
    (
        "billing_refund_create",
        "Create a refund for a billing invoice.",
    ),
    (
        "billing_invoice_list",
        "List the billing invoices of an account.",
    ),
    (
        "inventory_stock_adjust",
        "Adjust the stock level of an inventory item.",
    ),
    (
        "shipping_label_print",
        "Print a shipping label for an order.",
    ),
];

fn ranked(result: &sven_decision::DecisionResult) -> Vec<(String, f32)> {
    match &result.answer("tool").unwrap().answer {
        Answer::Ranked(d) => d
            .ranked
            .iter()
            .map(|c| (c.id.clone(), c.probability))
            .collect(),
        Answer::Unsupported { .. } => panic!("unsupported"),
    }
}

fn assert_valid_distribution(ranked: &[(String, f32)], n: usize) {
    assert_eq!(ranked.len(), n, "every option is ranked");
    let sum: f32 = ranked.iter().map(|(_, p)| p).sum();
    assert!(
        (sum - 1.0).abs() < 1e-3,
        "probabilities sum to one, got {sum}"
    );
    assert!(ranked.iter().all(|(_, p)| (0.0..=1.0).contains(p)));
}

#[tokio::test]
async fn the_heuristic_picks_the_option_that_shares_the_most_distinctive_words() {
    let provider = HeuristicProvider::new();
    let request = choice("print a shipping label for order 17", &OPTIONS);
    let result = provider.decide(&request).await.unwrap();
    let ranked = ranked(&result);
    assert_valid_distribution(&ranked, 4);
    assert_eq!(ranked[0].0, "shipping_label_print");
}

#[tokio::test]
async fn the_heuristic_does_not_depend_on_the_order_of_the_options() {
    let provider = HeuristicProvider::new();
    let mut reversed = OPTIONS;
    reversed.reverse();
    let a = provider
        .decide(&choice("refund the billing invoice", &OPTIONS))
        .await
        .unwrap();
    let b = provider
        .decide(&choice("refund the billing invoice", &reversed))
        .await
        .unwrap();
    assert_eq!(ranked(&a)[0].0, ranked(&b)[0].0);
    assert_eq!(ranked(&a)[0].0, "billing_refund_create");
}

#[tokio::test]
async fn the_heuristic_does_not_claim_to_know_a_proposition() {
    let provider = HeuristicProvider::new();
    let request = DecisionRequest {
        state: "anything".into(),
        questions: vec![Question::Probability(sven_decision::Probability {
            id: "p".into(),
            proposition: "The build is green.".into(),
        })],
        deadline: Duration::from_secs(1),
    };
    let result = provider.decide(&request).await.unwrap();
    let QuestionKind::Probability = result.answers[0].kind else {
        panic!("kind")
    };
    let Answer::Ranked(d) = &result.answers[0].answer else {
        panic!("ranked")
    };
    assert!(d.margin() < 1e-6, "an even answer, so the policy abstains");
}

#[tokio::test]
async fn the_random_control_is_valid_and_repeatable_for_a_seed() {
    let request = choice("anything at all", &OPTIONS);
    let a = RandomProvider::new(7).decide(&request).await.unwrap();
    let b = RandomProvider::new(7).decide(&request).await.unwrap();
    assert_eq!(ranked(&a), ranked(&b));
    assert_valid_distribution(&ranked(&a), 4);
}

#[tokio::test]
async fn the_random_control_differs_between_seeds_and_between_questions() {
    let request = choice("anything at all", &OPTIONS);
    let winners: Vec<String> = {
        let mut w = Vec::new();
        for seed in 0..32 {
            let r = RandomProvider::new(seed).decide(&request).await.unwrap();
            w.push(ranked(&r)[0].0.clone());
        }
        w
    };
    let distinct: std::collections::BTreeSet<_> = winners.iter().collect();
    assert!(
        distinct.len() > 1,
        "seeds must vary the winner: {winners:?}"
    );
    let other = RandomProvider::new(7)
        .decide(&choice("another state entirely", &OPTIONS))
        .await
        .unwrap();
    let same = RandomProvider::new(7).decide(&request).await.unwrap();
    assert_ne!(ranked(&other), ranked(&same));
}

#[tokio::test]
async fn the_random_control_answers_the_same_whatever_order_the_options_come_in() {
    // Re-asking in the opposite order must not be a way to sample twice.
    let mut reversed = OPTIONS;
    reversed.reverse();
    let a = RandomProvider::new(3)
        .decide(&choice("q", &OPTIONS))
        .await
        .unwrap();
    let b = RandomProvider::new(3)
        .decide(&choice("q", &reversed))
        .await
        .unwrap();
    assert_eq!(ranked(&a)[0].0, ranked(&b)[0].0);
}

#[tokio::test]
async fn the_oracle_picks_the_labelled_option_and_is_otherwise_even() {
    let provider = OracleProvider::new(vec![OracleLabel {
        decision_type: "tool_routing".into(),
        state_contains: "shipping label".into(),
        correct_option: "shipping_label_print".into(),
    }]);
    let hit = provider
        .decide(&choice("print a shipping label", &OPTIONS))
        .await
        .unwrap();
    let hit = ranked(&hit);
    assert_valid_distribution(&hit, 4);
    assert_eq!(hit[0].0, "shipping_label_print");
    assert!(hit[0].1 > 0.9);

    let miss = provider
        .decide(&choice("something else", &OPTIONS))
        .await
        .unwrap();
    let miss = ranked(&miss);
    assert!(miss[0].1 - miss[1].1 < 1e-6, "no label, no opinion");
}

#[tokio::test]
async fn the_oracle_ignores_a_label_whose_option_was_not_offered() {
    let provider = OracleProvider::new(vec![OracleLabel {
        decision_type: "tool_routing".into(),
        state_contains: "label".into(),
        correct_option: "not_offered".into(),
    }]);
    let result = provider
        .decide(&choice("print a label", &OPTIONS))
        .await
        .unwrap();
    let r = ranked(&result);
    assert!(r[0].1 - r[1].1 < 1e-6);
}
