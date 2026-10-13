// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The questions a harness asks a decision model.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::DecisionError;

/// Most questions one request may carry.
pub const MAX_QUESTIONS: usize = 64;
/// Most options one choice may offer.
pub const MAX_OPTIONS: usize = 255;
/// Fewest levels a score may have.
pub const MIN_SCORE_LEVELS: usize = 2;
/// Most levels a score may have.
pub const MAX_SCORE_LEVELS: usize = 10;
/// Largest state, in bytes.
pub const MAX_STATE_BYTES: usize = 1 << 20;

/// The shape of a question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Pick one of several options.
    Choice,
    /// Place on an ordered scale.
    Score,
    /// How likely a proposition is.
    Probability,
}

impl QuestionKind {
    /// The name used in diagnostics and trajectories.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Choice => "choice",
            Self::Score => "score",
            Self::Probability => "probability",
        }
    }
}

/// One option of a [`Choice`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub id: String,
    /// What the option means, when its id does not say it all.
    pub description: Option<String>,
}

/// Which of several options fits the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub instruction: String,
    /// In the order the caller lists them; an option's position is how an
    /// answer maps back onto the caller's list.
    pub options: Vec<ChoiceOption>,
}

/// How likely a proposition is, given the state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probability {
    pub id: String,
    pub proposition: String,
}

/// Where on an ordered scale the state falls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Score {
    pub id: String,
    pub instruction: String,
    /// Level descriptions, worst first.
    pub levels: Vec<String>,
}

/// A typed question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Question {
    Choice(Choice),
    Score(Score),
    Probability(Probability),
}

impl Question {
    /// The caller's name for the question; answers are matched by it.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Choice(q) => &q.id,
            Self::Score(q) => &q.id,
            Self::Probability(q) => &q.id,
        }
    }

    /// The question's shape.
    #[must_use]
    pub fn kind(&self) -> QuestionKind {
        match self {
            Self::Choice(_) => QuestionKind::Choice,
            Self::Score(_) => QuestionKind::Score,
            Self::Probability(_) => QuestionKind::Probability,
        }
    }

    /// The same question under another id.
    #[must_use]
    pub fn renamed(&self, id: String) -> Self {
        let mut question = self.clone();
        match &mut question {
            Self::Choice(q) => q.id = id,
            Self::Score(q) => q.id = id,
            Self::Probability(q) => q.id = id,
        }
        question
    }

    fn validate(&self) -> Result<(), String> {
        let id = self.id();
        match self {
            Self::Choice(q) => {
                if q.options.is_empty() || q.options.len() > MAX_OPTIONS {
                    return Err(format!(
                        "question {id:?}: a choice needs 1 to {MAX_OPTIONS} options, got {}",
                        q.options.len()
                    ));
                }
                let mut seen = BTreeSet::new();
                for option in &q.options {
                    if option.id.is_empty() || !seen.insert(option.id.as_str()) {
                        return Err(format!(
                            "question {id:?}: option ids must be non-empty and unique, got {:?}",
                            option.id
                        ));
                    }
                }
            }
            Self::Score(q) => {
                if !(MIN_SCORE_LEVELS..=MAX_SCORE_LEVELS).contains(&q.levels.len()) {
                    return Err(format!(
                        "question {id:?}: a score needs {MIN_SCORE_LEVELS} to {MAX_SCORE_LEVELS} levels, got {}",
                        q.levels.len()
                    ));
                }
            }
            Self::Probability(_) => {}
        }
        Ok(())
    }
}

/// Typed questions about one state, with a deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRequest {
    /// The situation, as text.
    pub state: String,
    pub questions: Vec<Question>,
    /// How long the caller will wait for the answer.
    pub deadline: Duration,
}

impl DecisionRequest {
    /// Checks the request against the limits the contract publishes.
    ///
    /// # Errors
    ///
    /// [`DecisionError::InvalidRequest`] naming the offending question.
    pub fn validate(&self) -> Result<(), DecisionError> {
        self.check().map_err(DecisionError::InvalidRequest)
    }

    fn check(&self) -> Result<(), String> {
        if self.deadline.is_zero() {
            return Err("the deadline must be greater than zero".into());
        }
        if self.state.len() > MAX_STATE_BYTES {
            return Err(format!(
                "the state is {} bytes; at most {MAX_STATE_BYTES} are accepted",
                self.state.len()
            ));
        }
        if self.questions.is_empty() || self.questions.len() > MAX_QUESTIONS {
            return Err(format!(
                "a request needs 1 to {MAX_QUESTIONS} questions, got {}",
                self.questions.len()
            ));
        }
        let mut seen = BTreeSet::new();
        for question in &self.questions {
            if question.id().is_empty() || !seen.insert(question.id()) {
                return Err(format!(
                    "question ids must be non-empty and unique, got {:?}",
                    question.id()
                ));
            }
            question.validate()?;
        }
        Ok(())
    }
}
