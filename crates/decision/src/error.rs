// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! How a decision provider fails.

use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

/// Why a decision could not be obtained. Every variant is a distinct class a
/// caller can react to: the remedy for a model that is not installed is not
/// the remedy for a provider that is not running.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum DecisionError {
    /// The provider could not be reached at all.
    #[error("the decision provider is unreachable: {0}")]
    Unreachable(String),
    /// The provider is up but the model is not installed on it.
    #[error("decision model '{model}' is not installed: {detail}")]
    ModelNotInstalled { model: String, detail: String },
    /// The model is installed but could not be loaded.
    #[error("decision model '{model}' failed to load: {detail}")]
    FailedToLoad { model: String, detail: String },
    /// The model cannot answer what was asked, or does not meet the required
    /// quality (for example it is not validated).
    #[error("unsupported by the decision model: {0}")]
    UnsupportedCapability(String),
    /// The provider did not answer within the deadline.
    #[error("the decision model did not answer within {} ms", .deadline.as_millis())]
    Timeout { deadline: Duration },
    /// The provider answered with something that is not a decision.
    #[error("invalid decision response: {0}")]
    InvalidResponse(String),
    /// The request is outside what the contract allows; a defect of the
    /// caller, never retried.
    #[error("invalid decision request: {0}")]
    InvalidRequest(String),
}

/// The class of a [`DecisionError`], without its detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    Unreachable,
    ModelNotInstalled,
    FailedToLoad,
    UnsupportedCapability,
    Timeout,
    InvalidResponse,
    InvalidRequest,
}

impl ErrorClass {
    /// The stable name used in diagnostics and trajectories.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Unreachable => "unreachable",
            Self::ModelNotInstalled => "model_not_installed",
            Self::FailedToLoad => "failed_to_load",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::Timeout => "timeout",
            Self::InvalidResponse => "invalid_response",
            Self::InvalidRequest => "invalid_request",
        }
    }
}

impl DecisionError {
    /// The class of this error.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Unreachable(_) => ErrorClass::Unreachable,
            Self::ModelNotInstalled { .. } => ErrorClass::ModelNotInstalled,
            Self::FailedToLoad { .. } => ErrorClass::FailedToLoad,
            Self::UnsupportedCapability(_) => ErrorClass::UnsupportedCapability,
            Self::Timeout { .. } => ErrorClass::Timeout,
            Self::InvalidResponse(_) => ErrorClass::InvalidResponse,
            Self::InvalidRequest(_) => ErrorClass::InvalidRequest,
        }
    }
}
