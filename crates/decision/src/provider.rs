// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The seam a decision model provider implements.

use async_trait::async_trait;

use crate::{DecisionError, DecisionRequest, DecisionResult};

/// A source of decisions: a model reached in-process, over a wire, or
/// scripted for a test.
///
/// An implementation honours the request's deadline and maps every failure to
/// a [`DecisionError`] class; it never substitutes an answer of its own.
#[async_trait]
pub trait DecisionProvider: Send + Sync {
    /// Answers the questions of `request`.
    ///
    /// # Errors
    ///
    /// A [`DecisionError`] naming the class of failure.
    async fn decide(&self, request: &DecisionRequest) -> Result<DecisionResult, DecisionError>;
}
