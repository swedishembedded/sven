// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The decision runtime.
//!
//! A decision model is a small, non-generative model that answers typed
//! questions about a state: which of these options, how likely is this
//! proposition. It only advises. Whether an answer is acted on is decided by
//! deterministic policy, and the state machine stays
//! authoritative.
//!
//! * [`DecisionRequest`] / [`Question`]: what is asked.
//! * [`DecisionResult`] / [`Distribution`]: what comes back, the whole ranked
//!   distribution of every question and never only its winner.
//! * [`DecisionProvider`]: the async seam a model provider implements.
//! * [`DecisionError`]: how a provider fails, in distinct named classes.
//! * [`MockDecisionProvider`]: a scripted provider for tests.

mod batch;
mod cache;
mod error;
mod mock;
pub mod policy;
mod provider;
mod question;
mod result;

pub use batch::{decide_batched, Batched};
pub use cache::{CacheKey, DecisionCache};
pub use error::{DecisionError, ErrorClass};
pub use mock::MockDecisionProvider;
pub use provider::DecisionProvider;
pub use question::{
    Choice, ChoiceOption, DecisionRequest, Probability, Question, QuestionKind, Score, MAX_OPTIONS,
    MAX_QUESTIONS, MAX_SCORE_LEVELS, MAX_STATE_BYTES, MIN_SCORE_LEVELS,
};
pub use result::{Answer, Candidate, DecisionResult, Distribution, ModelIdentity, QuestionAnswer};
