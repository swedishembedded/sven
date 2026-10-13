// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A deterministic rule-based provider: what a decision model has to beat.

use std::collections::BTreeSet;

use async_trait::async_trait;
use sven_decision::{Choice, DecisionError, DecisionProvider, DecisionRequest, DecisionResult};

use super::answer_request;

/// Words that carry no signal about which option fits.
const STOP_WORDS: [&str; 22] = [
    "a", "an", "the", "of", "for", "to", "and", "or", "in", "on", "with", "by", "is", "it", "this",
    "that", "task", "me", "my", "all", "get", "from",
];

/// How much more a word in an option's name counts than one in its description.
const NAME_WEIGHT: f64 = 2.0;

/// Sharpness of the softmax over the option scores.
const SHARPNESS: f64 = 1.5;

/// Scores each option by the words it shares with the state, each weighted by
/// how few of the offered options contain it (a word every option has says
/// nothing), and counting a word of the option's name double. The answer does
/// not depend on the order the options come in.
#[derive(Debug, Default, Clone, Copy)]
pub struct HeuristicProvider;

impl HeuristicProvider {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Lowercase words of `text`, simply stemmed, without stop words.
fn words(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .filter(|w| !STOP_WORDS.contains(&w.as_str()))
        .map(|w| stem(&w))
        .collect()
}

fn stem(word: &str) -> String {
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(root) = word.strip_suffix(suffix) {
            if root.len() >= 3 {
                return root.to_string();
            }
        }
    }
    word.to_string()
}

fn weigh(request: &DecisionRequest, choice: &Choice) -> Vec<f64> {
    let state = words(
        request
            .state
            .strip_prefix("Task:")
            .unwrap_or(&request.state),
    );
    let per_option: Vec<(BTreeSet<String>, BTreeSet<String>)> = choice
        .options
        .iter()
        .map(|o| (words(&o.id), words(o.description.as_deref().unwrap_or(""))))
        .collect();
    let n = per_option.len() as f64;
    let document_frequency = |word: &str| {
        per_option
            .iter()
            .filter(|(name, description)| name.contains(word) || description.contains(word))
            .count() as f64
    };
    per_option
        .iter()
        .map(|(name, description)| {
            let score: f64 = state
                .iter()
                .map(|word| {
                    let idf = (1.0 + n / document_frequency(word).max(1.0)).ln();
                    if name.contains(word) {
                        NAME_WEIGHT * idf
                    } else if description.contains(word) {
                        idf
                    } else {
                        0.0
                    }
                })
                .sum();
            (SHARPNESS * score).exp()
        })
        .collect()
}

#[async_trait]
impl DecisionProvider for HeuristicProvider {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        answer_request("ablation/heuristic", request, weigh)
    }
}
