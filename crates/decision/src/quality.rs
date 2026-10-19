// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for measuring small decision models
// inside agent runtimes for its clients. If your team needs expertise in
// calibrating and gating model advice then you can procure our services by
// sending an email to info@swedishembedded.com.
//! The quality of a decision point, read off its traces.
//!
//! Every decision is traced ([`sven_vocab::DecisionTraceRecord`]): the policy
//! that asked, a digest of what it asked, what was accepted and with what
//! probability, or why the policy abstained. [`point_quality`] groups the
//! traces by policy. Without labels it reports how often a point answered, why
//! it abstained and whether the same inputs always got the same answer. With
//! [`Labels`] (the answer a decision should have given, by inputs digest) it
//! also reports how often an accepted answer was right, how much of the labelled
//! work it answered at all, and whether the probabilities it stated match how
//! often it was right.
//!
//! The labels come from somewhere else (an oracle, a verifier's verdict on the
//! outcome); nothing here invents them, and a decision without one is counted
//! but not scored.

use std::collections::{BTreeMap, BTreeSet};

use sven_vocab::DecisionTraceRecord;

/// Calibration bins: probability in `[i/10, (i+1)/10)`, the last closed.
const BINS: usize = 10;

/// The answers decisions should have given, by inputs digest.
#[derive(Debug, Clone, Default)]
pub struct Labels(BTreeMap<String, String>);

impl Labels {
    /// Labels from `inputs_digest -> the correct selected option`.
    #[must_use]
    pub fn new(labels: BTreeMap<String, String>) -> Self {
        Self(labels)
    }
}

/// One bin of the calibration curve.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationBin {
    pub lower: f64,
    pub upper: f64,
    /// Accepted, labelled answers in the bin.
    pub n: u32,
    /// The mean probability they stated.
    pub mean_probability: f64,
    /// The share of them that were right.
    pub accuracy: f64,
}

/// What one decision point did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PointQuality {
    pub decisions: u32,
    pub accepted: u32,
    pub abstained: u32,
    /// Accepted answers that changed what happened (not shadow, not advisory-only).
    pub applied: u32,
    pub abstain_by_reason: BTreeMap<String, u32>,
    /// Inputs digests that got more than one different answer.
    pub unstable_inputs: u32,
    /// Decisions that have a label.
    pub labelled: u32,
    /// Accepted answers that matched the label.
    pub correct: u32,
    /// Accepted answers that did not.
    pub wrong: u32,
    /// `correct / (correct + wrong)`.
    pub accuracy_on_accepted: Option<f64>,
    /// The share of labelled decisions that were answered at all.
    pub coverage: Option<f64>,
    /// Calibration bins that hold at least one answer, lowest first.
    pub calibration: Vec<CalibrationBin>,
    /// The sum over bins of `|accuracy - mean probability|`, weighted by the bin's
    /// share of answers.
    pub expected_calibration_error: Option<f64>,
}

/// The quality of each decision point (policy) among `traces`.
#[must_use]
pub fn point_quality(
    traces: &[DecisionTraceRecord],
    labels: &Labels,
) -> BTreeMap<String, PointQuality> {
    let mut by_policy: BTreeMap<&str, Vec<&DecisionTraceRecord>> = BTreeMap::new();
    for trace in traces {
        by_policy.entry(&trace.policy).or_default().push(trace);
    }
    by_policy
        .into_iter()
        .map(|(policy, traces)| (policy.to_owned(), one_point(&traces, labels)))
        .collect()
}

fn one_point(traces: &[&DecisionTraceRecord], labels: &Labels) -> PointQuality {
    let mut q = PointQuality::default();
    let mut answers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut bins = [(0u32, 0.0f64, 0u32); BINS];
    for trace in traces {
        q.decisions += 1;
        let label = labels.0.get(&trace.inputs_digest);
        q.labelled += u32::from(label.is_some());
        if trace.verdict == "accept" {
            q.accepted += 1;
            q.applied += u32::from(trace.applied);
            if let Some(selected) = trace.selected.as_deref() {
                answers
                    .entry(&trace.inputs_digest)
                    .or_default()
                    .insert(selected);
            }
            if let (Some(label), Some(selected)) = (label, trace.selected.as_deref()) {
                let right = label == selected;
                if right {
                    q.correct += 1;
                } else {
                    q.wrong += 1;
                }
                if let Some(p) = trace.probability {
                    // f32 0.9 is slightly below 0.9: round so a stated edge lands in its bin.
                    let scaled = (f64::from(p) * BINS as f64 * 1e6).round() / 1e6;
                    let bin = (scaled as usize).min(BINS - 1);
                    bins[bin].0 += 1;
                    bins[bin].1 += f64::from(p);
                    bins[bin].2 += u32::from(right);
                }
            }
        } else {
            q.abstained += 1;
            let reason = trace
                .abstain_reason
                .clone()
                .unwrap_or_else(|| "unspecified".into());
            *q.abstain_by_reason.entry(reason).or_default() += 1;
        }
    }
    q.unstable_inputs =
        u32::try_from(answers.values().filter(|a| a.len() > 1).count()).unwrap_or(u32::MAX);
    let scored = q.correct + q.wrong;
    q.accuracy_on_accepted = (scored > 0).then(|| f64::from(q.correct) / f64::from(scored));
    q.coverage = (q.labelled > 0).then(|| f64::from(scored) / f64::from(q.labelled));
    let total: u32 = bins.iter().map(|b| b.0).sum();
    for (i, (n, sum, right)) in bins.iter().enumerate() {
        if *n == 0 {
            continue;
        }
        let n_f = f64::from(*n);
        q.calibration.push(CalibrationBin {
            lower: i as f64 / BINS as f64,
            upper: (i + 1) as f64 / BINS as f64,
            n: *n,
            mean_probability: sum / n_f,
            accuracy: f64::from(*right) / n_f,
        });
    }
    q.expected_calibration_error = (total > 0).then(|| {
        q.calibration
            .iter()
            .map(|b| f64::from(b.n) / f64::from(total) * (b.accuracy - b.mean_probability).abs())
            .sum()
    });
    q
}
