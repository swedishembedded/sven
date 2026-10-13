// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A scripted provider for tests.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{DecisionError, DecisionProvider, DecisionRequest, DecisionResult};

/// Answers from a queue of scripted results, in order, and records every
/// request it was asked. An error pushed with [`Self::push_error`] is the
/// failure injection; an exhausted script is [`DecisionError::Unreachable`],
/// so a test that asks more than it scripted fails loudly.
pub struct MockDecisionProvider {
    model: String,
    script: Mutex<VecDeque<Result<DecisionResult, DecisionError>>>,
    requests: Mutex<Vec<DecisionRequest>>,
}

impl MockDecisionProvider {
    /// A mock that stands in for the model `model`.
    #[must_use]
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            script: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// The model id this mock stands in for.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Scripts the next result.
    pub fn push_result(&self, result: DecisionResult) {
        self.script.lock().unwrap().push_back(Ok(result));
    }

    /// Scripts the next call to fail with `error`.
    pub fn push_error(&self, error: DecisionError) {
        self.script.lock().unwrap().push_back(Err(error));
    }

    /// Every request received so far, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<DecisionRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl DecisionProvider for MockDecisionProvider {
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError> {
        self.requests.lock().unwrap().push(request.clone());
        self.script.lock().unwrap().pop_front().unwrap_or_else(|| {
            Err(DecisionError::Unreachable(
                "the mock script is exhausted".into(),
            ))
        })
    }
}
