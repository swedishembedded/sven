// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The benchmark's providers, reached the way sven reaches a real model: over
//! the Brain wire, through the product's own transport.

use std::sync::Arc;
use std::time::Duration;

use sven_ablation::provider_server::ProviderServer;
use sven_ablation::providers::HeuristicProvider;
use sven_decision::brain::{BrainProvider, BrainSettings, MinimumQuality};
use sven_decision::{
    Answer, Choice, ChoiceOption, DecisionProvider, DecisionRequest, ModelState, Question,
};

fn request(task: &str) -> DecisionRequest {
    DecisionRequest {
        state: format!("Task: {task}"),
        questions: vec![Question::Choice(Choice {
            id: "tool".into(),
            instruction: "Which option fits the task best?".into(),
            options: [
                (
                    "billing_refund_create",
                    "Create a refund for a billing invoice.",
                ),
                (
                    "shipping_label_print",
                    "Print a shipping label for an order.",
                ),
            ]
            .iter()
            .map(|(id, d)| ChoiceOption {
                id: (*id).into(),
                description: Some((*d).into()),
            })
            .collect(),
        })],
        deadline: Duration::from_secs(2),
    }
}

async fn client(server: &ProviderServer, model: &str) -> BrainProvider {
    let mut settings = BrainSettings::new(&server.url());
    settings.model = model.to_string();
    settings.minimum_quality = MinimumQuality::Validated;
    BrainProvider::new(settings).unwrap()
}

#[tokio::test]
async fn the_products_transport_gets_a_validated_model_and_real_answers() {
    let server = ProviderServer::start(Arc::new(HeuristicProvider::new()), "ablation/heuristic")
        .await
        .unwrap();
    let brain = client(&server, "auto").await;

    let models = brain.models(Duration::from_secs(2)).await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "ablation/heuristic");
    assert_eq!(models[0].state, ModelState::Ready);
    assert!(models[0].validated);

    let result = brain
        .decide(&request("print a shipping label"))
        .await
        .unwrap();
    assert_eq!(result.model.id, "ablation/heuristic");
    assert!(
        result.model.checkpoint.is_some(),
        "a checkpoint makes the answer cacheable"
    );
    let Answer::Ranked(d) = &result.answer("tool").unwrap().answer else {
        panic!("ranked")
    };
    assert_eq!(d.top().id, "shipping_label_print");
    assert_eq!(server.stats().decisions(), 1);
    assert_eq!(server.stats().questions(), 1);
}

#[tokio::test]
async fn a_model_the_server_does_not_serve_is_not_found() {
    let server = ProviderServer::start(Arc::new(HeuristicProvider::new()), "ablation/heuristic")
        .await
        .unwrap();
    let brain = client(&server, "someone/else").await;
    assert!(brain.decide(&request("anything")).await.is_err());
}
