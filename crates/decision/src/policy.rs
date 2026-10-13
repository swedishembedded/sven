// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for putting small decision models
// behind typed, testable contracts for its clients. If your team needs
// expertise in decision-model integration for agents then you can procure our
// services by sending an email to info@swedishembedded.com.
//! Decision policy: what a decision point is willing to accept from a model.
//!
//! The model only advises. Every decision point declares a [`DecisionPolicy`]
//! up front (the shape of question it asks, how much input it sends, what a
//! wrong answer costs, how sure the model must be, whether the answer may
//! touch security or an irreversible action, what happens on abstention), and
//! [`evaluate`] is the pure function that turns a model's answer into a
//! [`Verdict`]: accepted, or abstained from with an explicit reason. There is
//! no global threshold; each point owns its own.
//!
//! # A verdict never grants anything
//!
//! A [`Verdict`] carries a choice among the options the caller itself offered
//! and the evidence for it. It has no field for a capability, an approval or
//! a permission, and this crate cannot name those types: `architecture.toml`
//! forbids a path from `sven-decision` to the kernel and tool crates that
//! define them. A decision point whose [`Impact`] is not
//! [`Impact::Routine`] is [`DecisionPolicy::advisory_only`] by construction,
//! and its accepted verdicts say so: the caller may use them to order or
//! annotate, and the deterministic permission policy decides regardless.

use serde::Serialize;

use crate::{Answer, DecisionError, DecisionResult, ErrorClass, Question, QuestionKind};

/// What a wrong answer costs, relative to the decision point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cost {
    Low,
    Medium,
    High,
}

/// What the decision can reach if it is acted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Impact {
    /// Ordering, selection or caching: a wrong answer costs time or tokens.
    Routine,
    /// Permissions, egress or other trust boundaries.
    Security,
    /// An action that cannot be undone.
    Irreversible,
}

/// What the caller does when the policy abstains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackAction {
    /// Behave exactly as before the decision point existed.
    Legacy,
    /// Do nothing; the optimisation is skipped.
    Skip,
}

/// The one question shape a decision point asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AllowedShape {
    pub kind: QuestionKind,
    /// Most options per question; unused by a proposition.
    pub max_options: usize,
}

/// How sure the model must be, both of the winner and of the lead.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Thresholds {
    /// The winner's probability (or the model's own confidence, when it
    /// reports one) must reach this. In `(0, 1]`.
    pub min_confidence: f32,
    /// The winner must lead the runner-up by this much. In `[0, 1]`.
    pub min_margin: f32,
}

/// A decision point's declaration, before it is checked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PolicyDecl {
    /// Names the decision point in diagnostics and trajectories.
    pub label: String,
    pub shape: AllowedShape,
    /// Most tokens of state the decision point sends.
    pub max_input_tokens: u32,
    pub false_positive: Cost,
    pub false_negative: Cost,
    pub thresholds: Thresholds,
    pub impact: Impact,
    pub fallback: FallbackAction,
}

/// A checked [`PolicyDecl`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionPolicy(PolicyDecl);

impl DecisionPolicy {
    /// Checks `decl` and makes it a policy.
    ///
    /// # Errors
    ///
    /// A message naming the field when a threshold is outside its range, the
    /// label is empty or no input is allowed.
    pub fn new(decl: PolicyDecl) -> Result<Self, String> {
        let t = decl.thresholds;
        if decl.label.is_empty() {
            return Err("a policy needs a label".into());
        }
        if !(t.min_confidence > 0.0 && t.min_confidence <= 1.0) {
            return Err(format!(
                "policy {:?}: min_confidence must be in (0, 1], got {}",
                decl.label, t.min_confidence
            ));
        }
        if !(0.0..=1.0).contains(&t.min_margin) {
            return Err(format!(
                "policy {:?}: min_margin must be in [0, 1], got {}",
                decl.label, t.min_margin
            ));
        }
        if decl.max_input_tokens == 0 {
            return Err(format!(
                "policy {:?}: max_input_tokens must be positive",
                decl.label
            ));
        }
        Ok(Self(decl))
    }

    /// The declaration.
    #[must_use]
    pub fn decl(&self) -> &PolicyDecl {
        &self.0
    }

    /// The trace label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.0.label
    }

    /// The caller's fallback on abstention.
    #[must_use]
    pub fn fallback(&self) -> FallbackAction {
        self.0.fallback
    }

    /// Whether an accepted answer may only advise. True for every decision
    /// point whose impact is not routine; there is no way to declare
    /// otherwise.
    #[must_use]
    pub fn advisory_only(&self) -> bool {
        self.0.impact != Impact::Routine
    }

    /// The input budget actually usable: the policy's, capped by what the
    /// served model reports it holds.
    #[must_use]
    pub fn input_tokens_within(&self, model_limit: Option<u32>) -> u32 {
        model_limit.map_or(self.0.max_input_tokens, |m| m.min(self.0.max_input_tokens))
    }
}

/// The answer a policy accepted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selected {
    /// A candidate of the question's own list.
    Candidate {
        id: String,
        index: usize,
        probability: f32,
        margin: f32,
    },
    /// A proposition judged to hold or not to hold.
    Proposition { holds: bool, probability: f32 },
}

impl Selected {
    /// What was selected, as a string for traces: the option id, or whether
    /// the proposition holds.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Candidate { id, .. } => id.clone(),
            Self::Proposition { holds, .. } => holds.to_string(),
        }
    }
}

/// An accepted answer and the evidence for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Accepted {
    pub selected: Selected,
    /// The caller may only use this to order or annotate; see the module
    /// documentation.
    pub advisory_only: bool,
    /// The policy that accepted it.
    pub policy: String,
}

/// Why a policy declined an answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum AbstainReason {
    /// The winner (or the model's own confidence) is under the threshold.
    LowConfidence { confidence: f32, required: f32 },
    /// The winner does not lead the runner-up by enough.
    NearTie { margin: f32, required: f32 },
    /// The model picked something the caller never offered.
    UnsupportedCandidate { id: String },
    /// The model answered from a prefix of the state.
    Truncated,
    /// The model declined questions of this kind.
    UnsupportedQuestion,
    /// The result holds no answer to the question.
    MissingAnswer,
    /// The question is not of the shape the decision point allows.
    ShapeNotAllowed { detail: String },
    /// The provider failed.
    ProviderFailed { class: ErrorClass },
    /// The winner changed when the options were asked about in another order.
    OrderSensitive { first: String, second: String },
    /// The question does not fit the input budget, even trimmed.
    InputTooLarge { tokens: u32, budget: u32 },
}

/// The outcome of [`evaluate`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Accept(Accepted),
    Abstain(AbstainReason),
}

/// Judges the model's answer to `question` against `policy`.
#[must_use]
pub fn evaluate(policy: &DecisionPolicy, question: &Question, result: &DecisionResult) -> Verdict {
    match judge(policy, question, result) {
        Ok(selected) => Verdict::Accept(Accepted {
            selected,
            advisory_only: policy.advisory_only(),
            policy: policy.label().to_string(),
        }),
        Err(reason) => Verdict::Abstain(reason),
    }
}

/// The tokens `text` costs a decision model: one per three characters,
/// rounded up. Deliberately pessimistic, because a model that truncates its
/// input answers about something else.
#[must_use]
pub fn estimate_input_tokens(text: &str) -> u32 {
    u32::try_from(text.chars().count().div_ceil(3)).unwrap_or(u32::MAX)
}

/// Combines the verdicts on one question asked with its options in two
/// orders: accepted only when both accept the same option, with the weaker
/// of the two as the evidence. A winner that depends on the order is a
/// model reading position, not content.
#[must_use]
pub fn agree(first: &Verdict, second: &Verdict) -> Verdict {
    match (first, second) {
        (Verdict::Accept(a), Verdict::Accept(b)) => {
            let (x, y) = (a.selected.describe(), b.selected.describe());
            if x != y {
                return Verdict::Abstain(AbstainReason::OrderSensitive {
                    first: x,
                    second: y,
                });
            }
            let weaker = |s: &Selected| match s {
                Selected::Candidate { margin, .. } => *margin,
                Selected::Proposition { probability, .. } => *probability,
            };
            Verdict::Accept(if weaker(&a.selected) <= weaker(&b.selected) {
                a.clone()
            } else {
                b.clone()
            })
        }
        (Verdict::Abstain(reason), _) | (_, Verdict::Abstain(reason)) => {
            Verdict::Abstain(reason.clone())
        }
    }
}

/// The verdict for a call that failed: always an abstention.
#[must_use]
pub fn evaluate_error(error: &DecisionError) -> Verdict {
    Verdict::Abstain(AbstainReason::ProviderFailed {
        class: error.class(),
    })
}

fn judge(
    policy: &DecisionPolicy,
    question: &Question,
    result: &DecisionResult,
) -> Result<Selected, AbstainReason> {
    let shape = policy.decl().shape;
    if question.kind() != shape.kind {
        return Err(AbstainReason::ShapeNotAllowed {
            detail: format!(
                "asked a {} question, allowed {}",
                question.kind().name(),
                shape.kind.name()
            ),
        });
    }
    if let Question::Choice(choice) = question {
        if choice.options.len() > shape.max_options {
            return Err(AbstainReason::ShapeNotAllowed {
                detail: format!(
                    "{} options, allowed {}",
                    choice.options.len(),
                    shape.max_options
                ),
            });
        }
    }
    let answered = result
        .answer(question.id())
        .ok_or(AbstainReason::MissingAnswer)?;
    let Answer::Ranked(distribution) = &answered.answer else {
        return Err(AbstainReason::UnsupportedQuestion);
    };
    if result.state_truncated || distribution.state_truncated {
        return Err(AbstainReason::Truncated);
    }
    let top = distribution.top();
    if let Question::Choice(choice) = question {
        if !choice.options.iter().any(|o| o.id == top.id) {
            return Err(AbstainReason::UnsupportedCandidate { id: top.id.clone() });
        }
    }
    let t = policy.decl().thresholds;
    let confidence = distribution.confidence.unwrap_or(top.probability);
    if confidence < t.min_confidence {
        return Err(AbstainReason::LowConfidence {
            confidence,
            required: t.min_confidence,
        });
    }
    let margin = distribution.margin();
    if margin < t.min_margin {
        return Err(AbstainReason::NearTie {
            margin,
            required: t.min_margin,
        });
    }
    Ok(match question.kind() {
        QuestionKind::Probability => Selected::Proposition {
            holds: top.id == "true",
            probability: distribution.probability_of("true").unwrap_or(0.0),
        },
        QuestionKind::Choice | QuestionKind::Score => Selected::Candidate {
            id: top.id.clone(),
            index: top.index,
            probability: top.probability,
            margin,
        },
    })
}
