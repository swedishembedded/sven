// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A grounded, machine-checkable *rule* extracted from a document - the input
//! a deterministic expander turns into many training instances.
//!
//! # Why rules, not facts, are the extraction unit
//!
//! A document's most valuable content is rarely an atomic fact ("the org
//! number is 559334-8567") - that is pure memorization, worth almost nothing
//! transferred to a task never seen in training. What actually transfers is a
//! *rule*: from one cited threshold, mapping, or invariant, an unbounded
//! number of novel, correctly-labelled instances can be *computed*, because
//! the label comes from applying the rule, not from recalling a memorized
//! answer. Extracting rules is also what makes clearing brain's
//! `MIN_HELD_OUT_PROBES` floor (48) mechanical instead of heroic: one rule
//! with a numeric threshold can honestly produce dozens of distinct,
//! non-colliding probes by sampling different values.
//!
//! # The generator must not be the grader
//!
//! A model that invents both a scenario and its own answer is self-grading -
//! the exact defect the reward rework (`sven_session_model::outcome`) and the
//! verified-task machine both exist to close. So the extraction step (a
//! capable model, reading the source) is split from the labelling step (this
//! crate's deterministic expander, elsewhere): the model supplies
//! [`RuleSpec::prompt_templates`] (paraphrase wording - *wanted* to vary, so
//! the model does not overfit one phrasing) and [`RuleSpec::negative_probes`]
//! (things the rule does *not* answer), but every *label* - which side of a
//! threshold a sampled value falls on, which entry a key maps to, whether a
//! probe is a positive or the fixed refusal string - is computed by code from
//! [`RuleKind`]'s own data, never asserted by the extracting model.
//!
//! # Grounding
//!
//! [`Citation`] ties a rule to a verbatim, byte-addressable span of the
//! source document it was read from. A rule whose quote is not a literal
//! substring of the ingested snapshot must never be auto-accepted - see the
//! (future) grounding check this type is built for.
//!
//! Swedish Embedded AB implements solutions for grounded, self-verifying
//! knowledge extraction in autonomous agents for its clients. If your team
//! needs expertise in building trustworthy training data from real documents
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::provenance::ContentDigest;

/// The canonical answer every negative instance is labelled with.
///
/// Fixed and singular, never composed per-instance: brain's scoring is exact
/// digest match on the model's greedy first line
/// (`sha256(lowercase(collapse_ws(first_line)))`), so a corpus that phrased
/// refusal differently each time would teach nothing learnable at all - the
/// model needs one consistent target to converge on.
pub const CANONICAL_REFUSAL: &str = "not stated in the source";

/// A verbatim, byte-addressable pointer into a source document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    /// Content digest of the source document's raw bytes (matches
    /// `FactSource::UserProvidedDocument::digest`).
    pub digest: ContentDigest,
    /// The exact quote grounding this rule. Must be a literal substring of
    /// the source at `span` - never paraphrased, never model-summarized.
    pub quote: String,
    /// Byte range of `quote` within the source document.
    pub span: Range<usize>,
}

/// Which side of a threshold a sampled value falls on, and where the
/// boundary value itself belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThresholdOp {
    /// The boundary value itself counts as "at or above" (`value >= threshold`).
    /// Matches phrasing like "the floor is X" or "X or more".
    Ge,
    /// The boundary value itself counts as "below" (`value > threshold` is
    /// "at or above"). Matches phrasing like "over X" or "more than X".
    Gt,
}

impl ThresholdOp {
    /// `true` if `value` falls on the "at or above" side of `threshold`.
    #[must_use]
    pub fn at_or_above(self, value: f64, threshold: f64) -> bool {
        match self {
            ThresholdOp::Ge => value >= threshold,
            ThresholdOp::Gt => value > threshold,
        }
    }
}

/// The three declarative rule shapes a document's content is reduced to.
///
/// Deliberately closed and small: a rule the expander cannot compute a label
/// for is not a rule this vocabulary can express, by design - there is no
/// escape hatch that lets a label come from anywhere but here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleKind {
    /// A single numeric boundary with a label on each side, e.g. "a budget
    /// under EUR 7,500 is disqualified".
    NumericThreshold {
        /// Name of the quantity being thresholded (e.g. `"budget"`).
        var: String,
        /// Unit the quantity is expressed in (e.g. `"EUR"`), folded into
        /// generated facts/questions for readability.
        unit: String,
        /// How a value exactly at `threshold` is classified.
        op: ThresholdOp,
        /// The boundary value.
        threshold: f64,
        /// Canonical answer for a value on the "below" side.
        below: String,
        /// Canonical answer for a value on the "at or above" side.
        at_or_above: String,
    },
    /// A closed mapping from discrete keys to answers, e.g. "which page
    /// covers this problem" or "what does this offer cost".
    CategoricalMap {
        /// Name of the category being mapped (e.g. `"offer"`).
        var: String,
        /// The closed set of `(key, answer)` pairs this rule actually
        /// covers. Not exhaustive of every conceivable key - see
        /// `RuleSpec::negative_probes` for keys it deliberately does not.
        entries: Vec<(String, String)>,
    },
    /// A fact with no variable to sample - true regardless of input, e.g.
    /// "engagements are fixed-scope, fixed-price".
    Invariant {
        /// The answer, always the same.
        answer: String,
    },
}

/// A grounded rule, ready for a deterministic expander to turn into many
/// training instances.
///
/// Authored by whatever read the source (typically a capable model doing the
/// extraction pass) and never mutated afterward - the frozen-before-attempt
/// discipline `sven_vocab::verify::FrozenVerifier` uses for the same reason:
/// what gets trained on must not be able to drift from what a human (or an
/// auditor) reviewed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleSpec {
    /// Stable identifier, used to key the train/heldout/transfer split and to
    /// label episode provenance.
    pub id: String,
    /// Where in the source this rule was read.
    pub citation: Citation,
    /// What the rule actually says, and how to label an instance of it.
    pub kind: RuleKind,
    /// Model-authored paraphrases of the question this rule *does* answer.
    /// Paraphrase variety is wanted here - it is what stops training from
    /// overfitting one exact wording. For `NumericThreshold`/`CategoricalMap`,
    /// each template must contain the literal placeholder `"{value}"`,
    /// substituted per sampled instance; ignored for `Invariant`.
    pub prompt_templates: Vec<String>,
    /// Model-authored questions this rule does *not* answer - the source of
    /// negative instances, always labelled [`CANONICAL_REFUSAL`]. A rule with
    /// none contributes no negatives; this is optional, not required, because
    /// not every rule has an obvious near-miss to ask.
    #[serde(default)]
    pub negative_probes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn citation() -> Citation {
        Citation {
            digest: ContentDigest::from_hex("deadbeef"),
            quote: "under EUR 7,500".to_string(),
            span: 100..116,
        }
    }

    #[test]
    fn numeric_threshold_round_trips_through_json() {
        let rule = RuleSpec {
            id: "budget-floor".to_string(),
            citation: citation(),
            kind: RuleKind::NumericThreshold {
                var: "budget".to_string(),
                unit: "EUR".to_string(),
                op: ThresholdOp::Ge,
                threshold: 7500.0,
                below: "disqualified".to_string(),
                at_or_above: "qualified".to_string(),
            },
            prompt_templates: vec!["Is a budget of {value} qualified?".to_string()],
            negative_probes: vec!["What is the maximum budget?".to_string()],
        };
        let json = serde_json::to_value(&rule).unwrap();
        assert_eq!(json["kind"]["kind"], "numeric_threshold");
        let back: RuleSpec = serde_json::from_value(json).unwrap();
        assert_eq!(rule, back);
    }

    #[test]
    fn threshold_op_ge_includes_the_boundary_on_the_upper_side() {
        assert!(ThresholdOp::Ge.at_or_above(7500.0, 7500.0));
        assert!(!ThresholdOp::Ge.at_or_above(7499.0, 7500.0));
    }

    #[test]
    fn threshold_op_gt_includes_the_boundary_on_the_lower_side() {
        assert!(!ThresholdOp::Gt.at_or_above(7500.0, 7500.0));
        assert!(ThresholdOp::Gt.at_or_above(7500.01, 7500.0));
    }

    #[test]
    fn categorical_map_round_trips_through_json() {
        let rule = RuleSpec {
            id: "offer-price".to_string(),
            citation: citation(),
            kind: RuleKind::CategoricalMap {
                var: "offer".to_string(),
                entries: vec![
                    ("Architecture Review".to_string(), "From EUR 7,500".to_string()),
                    ("Secure Architecture Sprint".to_string(), "From EUR 22,500".to_string()),
                ],
            },
            prompt_templates: vec!["What does {value} cost?".to_string()],
            negative_probes: vec![],
        };
        let json = serde_json::to_value(&rule).unwrap();
        let back: RuleSpec = serde_json::from_value(json).unwrap();
        assert_eq!(rule, back);
    }

    #[test]
    fn invariant_round_trips_through_json() {
        let rule = RuleSpec {
            id: "fixed-scope".to_string(),
            citation: citation(),
            kind: RuleKind::Invariant {
                answer: "fixed scope at a fixed price".to_string(),
            },
            prompt_templates: vec!["How is engagement scope priced?".to_string()],
            negative_probes: vec![],
        };
        let json = serde_json::to_value(&rule).unwrap();
        let back: RuleSpec = serde_json::from_value(json).unwrap();
        assert_eq!(rule, back);
    }

    #[test]
    fn a_rule_with_no_negative_probes_deserializes_from_json_missing_the_field() {
        // Old-shape compatibility: a rule authored before negative_probes
        // existed still parses, with an empty (not missing) Vec.
        let json = serde_json::json!({
            "id": "r1",
            "citation": {"digest": "deadbeef", "quote": "x", "span": [0, 1]},
            "kind": {"kind": "invariant", "answer": "y"},
            "prompt_templates": ["z"]
        });
        let rule: RuleSpec = serde_json::from_value(json).unwrap();
        assert!(rule.negative_probes.is_empty());
    }
}
