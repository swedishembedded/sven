// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Deterministically expands one [`RuleSpec`] into many training instances.
//!
//! `expand(rule, seed)` is the only place a `RuleSpec`'s paraphrase templates
//! and negative probes turn into concrete `{fact, probe_question,
//! expected_answer}` triples. Every *label* here is computed from
//! [`RuleKind`]'s own data (`sven_vocab::rule`'s module doc explains why:
//! the generator must not be the grader) - this module never asks a model
//! whether an instance is correct, it derives the answer directly from the
//! rule that was already extracted and grounded.
//!
//! Determinism matters twice over: the same `(rule, seed)` must always
//! produce the same instances (so a re-run is reproducible and auditable),
//! and the train/heldout/transfer split must never be redrawn as the corpus
//! grows (Stage 6's curator note applies here too - re-drawing leaks
//! held-out score). Both properties come from using only fixed sampling
//! schedules and content-hash-derived split decisions, never a general
//! PRNG.
//!
//! Swedish Embedded AB implements solutions for grounded, reproducible
//! training-data generation in autonomous agents for its clients. If your
//! team needs expertise in closing the loop between real documents and a
//! trainable, honestly-labelled corpus then you can procure our services by
//! sending an email to info@swedishembedded.com.

use sha2::{Digest, Sha256};
use sven_vocab::rule::{RuleKind, RuleSpec, CANONICAL_REFUSAL};

/// Percentage of rules (by `rule_id`, not by instance) whose entire instance
/// set is held out of training - the actual transfer-generalization test,
/// since a rule application task the model never trained on requires it to
/// have learned the *rule*, not memorized this document's phrasing of it.
const TRANSFER_RULE_PCT: u64 = 20;
/// Of the remaining rules' instances, the percentage set aside as heldout -
/// same rules, unseen instances, measuring fit rather than generalization.
const HELDOUT_INSTANCE_PCT: u64 = 20;
/// Target negative-instance fraction of a rule's total output. ~25% per the
/// plan: without negatives, training on a fact corpus reliably increases
/// confident fabrication on questions the source never answered.
const NEGATIVE_FRACTION: f64 = 0.25;

/// Where one expanded instance belongs in the corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Split {
    /// Becomes a fact in the pending-facts ledger.
    Train,
    /// Same rule as some `Train` instances, but this exact instance was
    /// never trained on - measures fit.
    Heldout,
    /// This instance's *entire rule* was excluded from training - measures
    /// whether rule application generalizes to a rule never seen at all.
    /// The headline transfer number.
    Transfer,
}

/// One expanded training instance.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    /// The statement to train on (a Brain document-study cycle row's `fact`).
    pub fact: String,
    /// The question a frozen probe asks - never trained on.
    pub probe_question: String,
    /// The answer `probe_question` must elicit, exactly (Brain's scoring is
    /// exact digest match on the model's greedy first line - no punctuation,
    /// no preamble).
    pub expected_answer: String,
    /// Which corpus split this instance belongs to.
    pub split: Split,
    /// The rule this instance was expanded from.
    pub rule_id: String,
}

/// Expands `rule` into its full set of training instances. Deterministic:
/// the same `(rule, seed)` always produces the same `Vec<Instance>` in the
/// same order.
#[must_use]
pub fn expand(rule: &RuleSpec, seed: u64) -> Vec<Instance> {
    let transfer = derive_u64(seed, &rule.id, "rule-split") % 100 < TRANSFER_RULE_PCT;

    let mut positives = expand_positives(rule);
    let target_negatives = ((positives.len() as f64) * NEGATIVE_FRACTION
        / (1.0 - NEGATIVE_FRACTION).max(f64::EPSILON))
    .round() as usize;
    let negatives = expand_negatives(rule, target_negatives);

    let mut instances = Vec::with_capacity(positives.len() + negatives.len());
    instances.append(&mut positives);
    instances.extend(negatives);

    for (idx, instance) in instances.iter_mut().enumerate() {
        instance.split = if transfer {
            Split::Transfer
        } else if derive_u64(seed, &rule.id, &format!("instance-{idx}")) % 100
            < HELDOUT_INSTANCE_PCT
        {
            Split::Heldout
        } else {
            Split::Train
        };
    }
    instances
}

/// The label-carrying instances: one per `(sample point, template)` pair for
/// `NumericThreshold`, one per `(entry, template)` pair for `CategoricalMap`,
/// one per template for `Invariant`. `split` is left at a placeholder and
/// overwritten by the caller once the full instance count is known.
fn expand_positives(rule: &RuleSpec) -> Vec<Instance> {
    let mut out = Vec::new();
    match &rule.kind {
        RuleKind::NumericThreshold {
            var,
            unit,
            op,
            threshold,
            below,
            at_or_above,
        } => {
            for value in numeric_samples(*threshold) {
                let at_or_above_side = op.at_or_above(value, *threshold);
                let answer = if at_or_above_side { at_or_above } else { below };
                let value_text = format_numeric(value);
                let fact = format!("A {var} of {value_text} {unit} is {answer}.");
                for template in &rule.prompt_templates {
                    out.push(Instance {
                        fact: fact.clone(),
                        probe_question: template
                            .replace("{value}", &format!("{value_text} {unit}")),
                        expected_answer: answer.clone(),
                        split: Split::Train,
                        rule_id: rule.id.clone(),
                    });
                }
            }
        }
        RuleKind::CategoricalMap { var, entries } => {
            for (key, answer) in entries {
                let fact = format!("For {var} '{key}': {answer}.");
                for template in &rule.prompt_templates {
                    out.push(Instance {
                        fact: fact.clone(),
                        probe_question: template.replace("{value}", key),
                        expected_answer: answer.clone(),
                        split: Split::Train,
                        rule_id: rule.id.clone(),
                    });
                }
            }
        }
        RuleKind::Invariant { answer } => {
            let fact = rule.citation.quote.clone();
            for template in &rule.prompt_templates {
                out.push(Instance {
                    fact: fact.clone(),
                    probe_question: template.clone(),
                    expected_answer: answer.clone(),
                    split: Split::Train,
                    rule_id: rule.id.clone(),
                });
            }
        }
    }
    out
}

/// Up to `target` negative instances, cycling `rule.negative_probes` if it is
/// shorter than `target`. Empty if the rule declared no negative probes at
/// all - not every rule has an obvious near-miss to ask.
fn expand_negatives(rule: &RuleSpec, target: usize) -> Vec<Instance> {
    if rule.negative_probes.is_empty() || target == 0 {
        return Vec::new();
    }
    let fact = rule.citation.quote.clone();
    (0..target)
        .map(|i| {
            let question = &rule.negative_probes[i % rule.negative_probes.len()];
            // A repeated probe (target > negative_probes.len()) must still be
            // a distinct question, or Brain's cross-batch probe-question
            // uniqueness assertion panics on the duplicate.
            let probe_question = if i < rule.negative_probes.len() {
                question.clone()
            } else {
                format!("{question} (#{})", i / rule.negative_probes.len() + 1)
            };
            Instance {
                fact: fact.clone(),
                probe_question,
                expected_answer: CANONICAL_REFUSAL.to_string(),
                split: Split::Train,
                rule_id: rule.id.clone(),
            }
        })
        .collect()
}

/// Fixed, deterministic sample points around `threshold`: small absolute
/// deltas right at the boundary (this is what actually tests "learned the
/// threshold" versus "memorized one cited number" - e.g. 7,499 vs 7,500) plus
/// relative multiples further out (tests the rule still holds away from the
/// exact phrasing it was cited from).
fn numeric_samples(threshold: f64) -> Vec<f64> {
    let near = [-2.0, -1.0, 0.0, 1.0, 2.0].iter().map(|d| threshold + d);
    let far = [0.1, 0.5, 1.5, 2.0, 5.0].iter().map(|m| threshold * m);
    near.chain(far).collect()
}

/// Renders a sample value the way it should read in generated text: whole
/// numbers with no trailing `.0`, since brain's scoring is exact-string.
fn format_numeric(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// A deterministic, seeded pseudo-value in `0..u64::MAX`, derived by hashing
/// `(seed, rule_id, salt)` - not a general PRNG (no state to seed once and
/// advance), just a stable function of its inputs, which is exactly the
/// "same input, same output, forever" property a never-redrawn split needs.
fn derive_u64(seed: u64, rule_id: &str, salt: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(seed.to_le_bytes());
    hasher.update(rule_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(salt.as_bytes());
    let digest = hasher.finalize();
    u64::from_le_bytes(
        digest[0..8]
            .try_into()
            .expect("sha256 digest is at least 8 bytes"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::provenance::ContentDigest;
    use sven_vocab::rule::{Citation, ThresholdOp};

    fn citation(quote: &str) -> Citation {
        Citation {
            digest: ContentDigest::from_hex("deadbeef"),
            quote: quote.to_string(),
            span: 0..quote.len(),
        }
    }

    fn numeric_rule() -> RuleSpec {
        RuleSpec {
            id: "budget-floor".to_string(),
            citation: citation("under EUR 7,500"),
            kind: RuleKind::NumericThreshold {
                var: "budget".to_string(),
                unit: "EUR".to_string(),
                op: ThresholdOp::Ge,
                threshold: 7500.0,
                below: "disqualified".to_string(),
                at_or_above: "qualified".to_string(),
            },
            prompt_templates: vec![
                "Is a budget of {value} qualified?".to_string(),
                "Does {value} clear the floor?".to_string(),
            ],
            negative_probes: vec!["What is the maximum budget?".to_string()],
        }
    }

    #[test]
    fn expansion_is_deterministic_for_the_same_seed() {
        let rule = numeric_rule();
        let a = expand(&rule, 42);
        let b = expand(&rule, 42);
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_seed_can_change_the_split_but_not_the_instance_count() {
        let rule = numeric_rule();
        let a = expand(&rule, 1);
        let b = expand(&rule, 2);
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn no_two_instances_from_one_rule_share_a_probe_question() {
        let rule = numeric_rule();
        let instances = expand(&rule, 7);
        let mut questions: Vec<&str> = instances
            .iter()
            .map(|i| i.probe_question.as_str())
            .collect();
        let before = questions.len();
        questions.sort_unstable();
        questions.dedup();
        assert_eq!(
            questions.len(),
            before,
            "every probe question must be unique within a rule"
        );
    }

    #[test]
    fn no_probe_question_is_a_substring_of_its_own_fact() {
        // Brain's own batch-admission rule (promote::document::FactBatch::new)
        // - normalized probe question must not occur inside the fact it is
        // paired with. Mirrored here so a malformed template is caught at
        // generation time, not as a brain-side panic.
        let rule = numeric_rule();
        for instance in expand(&rule, 3) {
            let norm = |s: &str| {
                s.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
            };
            assert!(
                !norm(&instance.fact).contains(&norm(&instance.probe_question)),
                "probe question leaked into fact: {:?} / {:?}",
                instance.probe_question,
                instance.fact
            );
        }
    }

    #[test]
    fn numeric_threshold_boundary_samples_are_labelled_correctly() {
        let rule = numeric_rule();
        let instances = expand(&rule, 5);
        let at_7499 = instances
            .iter()
            .find(|i| i.fact.contains("7499"))
            .expect("7499 sampled");
        assert_eq!(at_7499.expected_answer, "disqualified");
        let at_7500 = instances
            .iter()
            .find(|i| i.fact.contains("7500 EUR"))
            .expect("7500 sampled");
        assert_eq!(at_7500.expected_answer, "qualified");
    }

    #[test]
    fn categorical_map_produces_one_instance_per_entry_per_template() {
        let rule = RuleSpec {
            id: "offer-price".to_string(),
            citation: citation("From EUR 7,500"),
            kind: RuleKind::CategoricalMap {
                var: "offer".to_string(),
                entries: vec![
                    (
                        "Architecture Review".to_string(),
                        "From EUR 7,500".to_string(),
                    ),
                    (
                        "Secure Architecture Sprint".to_string(),
                        "From EUR 22,500".to_string(),
                    ),
                ],
            },
            prompt_templates: vec!["What does {value} cost?".to_string()],
            negative_probes: vec![],
        };
        let positives: Vec<_> = expand(&rule, 1)
            .into_iter()
            .filter(|i| i.expected_answer != CANONICAL_REFUSAL)
            .collect();
        assert_eq!(positives.len(), 2, "2 entries x 1 template");
    }

    #[test]
    fn a_rule_with_no_negative_probes_produces_no_negatives() {
        let rule = RuleSpec {
            id: "fixed-scope".to_string(),
            citation: citation("fixed scope at a fixed price"),
            kind: RuleKind::Invariant {
                answer: "fixed scope at a fixed price".to_string(),
            },
            prompt_templates: vec!["How is scope priced?".to_string()],
            negative_probes: vec![],
        };
        let instances = expand(&rule, 9);
        assert!(instances
            .iter()
            .all(|i| i.expected_answer != CANONICAL_REFUSAL));
    }

    #[test]
    fn negatives_are_roughly_a_quarter_of_the_total_when_available() {
        let rule = numeric_rule();
        let instances = expand(&rule, 11);
        let negatives = instances
            .iter()
            .filter(|i| i.expected_answer == CANONICAL_REFUSAL)
            .count();
        let ratio = negatives as f64 / instances.len() as f64;
        assert!(
            (0.15..=0.35).contains(&ratio),
            "negative ratio {ratio} out of the expected band"
        );
    }

    #[test]
    fn a_transfer_rule_puts_every_instance_in_transfer() {
        // Seeds are searched, not asserted for a specific value, since the
        // exact seed->bucket mapping is an implementation detail; this pins
        // the *property* (a rule is all-or-nothing for transfer), not one
        // magic seed.
        let rule = numeric_rule();
        let transfer_seed = (0..200u64)
            .find(|&s| expand(&rule, s).iter().all(|i| i.split == Split::Transfer))
            .expect("some seed puts this rule entirely in transfer");
        let instances = expand(&rule, transfer_seed);
        assert!(instances.iter().all(|i| i.split == Split::Transfer));
    }

    #[test]
    fn a_non_transfer_rule_has_both_train_and_heldout_instances() {
        let rule = numeric_rule();
        let non_transfer_seed = (0..200u64)
            .find(|&s| expand(&rule, s).iter().all(|i| i.split != Split::Transfer))
            .expect("some seed keeps this rule out of transfer");
        let instances = expand(&rule, non_transfer_seed);
        assert!(instances.iter().any(|i| i.split == Split::Train));
        assert!(instances.iter().any(|i| i.split == Split::Heldout));
    }
}
