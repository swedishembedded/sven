// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The recovery-aware part of the report.
//!
//! A recovery run is the same task and variant with a scripted model that
//! fetches what a shortened result left out when the question names it (see
//! [`crate::task::Recovery`]). The main tables are made of runs whose model
//! never asks and stay as they are; this section sets beside them, per suite
//! and variant, the evidence in the request when the question was first put
//! (what the harness alone delivers), the evidence after the model fetched,
//! and the tokens of the whole task including the fetches.
//!
//! The gates are decided a second time on the *recovery-aware measurement*:
//! the main runs with each run that has a recovery run replaced by it. A
//! third gate asks for the evidence the harness delivers without the model
//! asking, which is the one a real session cannot rely on a model for.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::gates::{evaluate_gates, CRITICAL_RECALL_FLOOR};
use crate::matrix::Matrix;
use crate::record::RunRecord;
use crate::report::{GateGrid, GateResult, GateStatus, View, BOOTSTRAP_SEED};
use crate::runner::RECOVERY_PROBE;
use crate::stats::{geometric_mean_ratio, mean, median, Interval};
use crate::task::Suite;

/// One variant in one suite, with a scripted model that recovers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryCell {
    pub variant: String,
    pub runs: usize,
    pub passes: usize,
    /// Critical evidence in the request that first carried the final
    /// question, pooled over runs.
    pub recall_without_recovery: Option<f64>,
    /// Critical evidence in the request the model answered from.
    pub recall_after_recovery: Option<f64>,
    pub tokens_total_mean: Option<f64>,
    /// Baseline (main run) / this run, geometric mean over tasks.
    pub r_tokens: Option<Interval>,
    pub r_tokens_median: Option<f64>,
    /// Expansions the model called while answering the final question.
    pub expansions_mean: Option<f64>,
    /// Estimated tokens those expansions brought back, over the whole task.
    pub expanded_tokens_mean: Option<f64>,
}

/// One suite's recovery cells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoverySuite {
    pub suite: Suite,
    pub cells: Vec<RecoveryCell>,
}

/// The recovery-aware section of a report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub suites: Vec<RecoverySuite>,
    /// Gates on the recovery-aware measurement, per non-baseline variant.
    pub gates: Vec<GateGrid>,
}

fn is_recovery(run: &&RunRecord) -> bool {
    run.probe.as_deref() == Some(RECOVERY_PROBE) && run.completed()
}

fn ratio(visible: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| visible as f64 / total as f64)
}

fn cell(
    view: &View<'_>,
    baseline: &str,
    variant: &str,
    suite: Suite,
    mine: &[&RunRecord],
) -> RecoveryCell {
    let runs: Vec<&RunRecord> = mine
        .iter()
        .copied()
        .filter(|r| r.variant.id == variant && r.suite == suite)
        .collect();
    let (mut before, mut after, mut total) = (0, 0, 0);
    for r in &runs {
        if let Some(s) = r.score.as_ref().and_then(|s| s.recovery.as_ref()) {
            before += s.visible_before;
            after += s.visible_after;
            total += s.critical_total;
        }
    }
    let base: BTreeMap<&str, f64> = view
        .runs(baseline)
        .into_iter()
        .filter_map(|r| Some((r.task_id.as_str(), r.tokens.as_ref()?.total as f64)))
        .collect();
    let ratios: Vec<f64> = runs
        .iter()
        .filter_map(|r| Some(base.get(r.task_id.as_str())? / r.tokens.as_ref()?.total as f64))
        .collect();
    let tokens: Vec<f64> = runs
        .iter()
        .filter_map(|r| r.tokens.as_ref().map(|t| t.total as f64))
        .collect();
    let expansions: Vec<f64> = runs
        .iter()
        .filter_map(|r| Some(r.score.as_ref()?.recovery.as_ref()?.expansions as f64))
        .collect();
    let expanded: Vec<f64> = runs
        .iter()
        .filter_map(|r| r.tokens.as_ref().map(|t| t.recovery_tokens as f64))
        .collect();
    RecoveryCell {
        variant: variant.to_string(),
        runs: runs.len(),
        passes: runs
            .iter()
            .filter(|r| r.score.as_ref().is_some_and(|s| s.passed))
            .count(),
        recall_without_recovery: ratio(before, total),
        recall_after_recovery: ratio(after, total),
        tokens_total_mean: mean(&tokens),
        r_tokens: geometric_mean_ratio(
            &ratios,
            BOOTSTRAP_SEED,
            &format!("recovery/{}/{variant}", suite.name()),
        ),
        r_tokens_median: median(&ratios),
        expansions_mean: mean(&expansions),
        expanded_tokens_mean: mean(&expanded),
    }
}

/// The main runs with each run that has a recovery run replaced by it.
fn recovery_aware(runs: &[RunRecord]) -> Vec<&RunRecord> {
    let recovered: BTreeMap<(&str, &str), &RunRecord> = runs
        .iter()
        .filter(is_recovery)
        .map(|r| ((r.variant.id.as_str(), r.task_id.as_str()), r))
        .collect();
    runs.iter()
        .filter(|r| r.probe.is_none())
        .map(|r| {
            let own = recovered.get(&(r.variant.id.as_str(), r.task_id.as_str()));
            own.copied().unwrap_or(r)
        })
        .collect()
}

/// Critical evidence the harness delivered without the model asking: for a
/// recovery run, the request that first carried the final question.
fn recall_without_asking(runs: &[&RunRecord]) -> GateResult {
    const GATE: &str = "critical evidence recall >= 99% without the model asking";
    let (mut visible, mut total) = (0, 0);
    for r in runs {
        let Some(score) = r.score.as_ref() else {
            continue;
        };
        if let Some(rec) = &score.recovery {
            visible += rec.visible_before;
            total += rec.critical_total;
        } else if let Some(e) = &score.evidence {
            visible += e.critical_visible;
            total += e.critical_total;
        }
    }
    let Some(recall) = ratio(visible, total) else {
        return GateResult {
            gate: GATE.into(),
            status: GateStatus::NotMeasured,
            detail: "no run carried critical evidence".into(),
        };
    };
    GateResult {
        gate: GATE.into(),
        status: if recall >= CRITICAL_RECALL_FLOOR {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        detail: format!(
            "{visible} of {total} critical items were in the request when the question was first put ({:.2}%)",
            recall * 100.0
        ),
    }
}

/// The recovery section for `runs`, or `None` when no recovery run exists.
#[must_use]
pub(crate) fn build(
    matrix: &Matrix,
    runs: &[RunRecord],
    view: &View<'_>,
) -> Option<RecoveryReport> {
    let mine: Vec<&RunRecord> = runs.iter().filter(is_recovery).collect();
    if mine.is_empty() {
        return None;
    }
    let baseline = matrix.baseline().id.clone();
    let mut suites: Vec<Suite> = mine.iter().map(|r| r.suite).collect();
    suites.sort();
    suites.dedup();
    let suite_reports = suites
        .iter()
        .map(|suite| RecoverySuite {
            suite: *suite,
            cells: matrix
                .variants
                .iter()
                .map(|v| cell(view, &baseline, &v.id, *suite, &mine))
                .collect(),
        })
        .collect();

    let aware: Vec<RunRecord> = recovery_aware(runs)
        .into_iter()
        .map(|r| RunRecord {
            probe: None,
            ..r.clone()
        })
        .collect();
    let gates = matrix
        .variants
        .iter()
        .filter(|v| v.id != baseline)
        .map(|v| {
            let mut gates: Vec<GateResult> = evaluate_gates(&aware, &baseline, &v.id)
                .into_iter()
                .filter(|g| {
                    g.gate.starts_with("token reduction")
                        || g.gate.starts_with("no material success")
                        || g.gate.starts_with("critical evidence recall")
                })
                .collect();
            let own: Vec<&RunRecord> = aware.iter().filter(|r| r.variant.id == v.id).collect();
            gates.push(recall_without_asking(&own));
            GateGrid {
                variant: v.id.clone(),
                gates,
            }
        })
        .collect();
    Some(RecoveryReport {
        suites: suite_reports,
        gates,
    })
}
