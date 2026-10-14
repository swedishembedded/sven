// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The release gates.
//!
//! Each gate is decided from the run records for one candidate variant
//! against the baseline, and is `PASS`, `FAIL` or `NOT-MEASURED`. A gate
//! that lacks the runs to be decided is `NOT-MEASURED`; absence of evidence
//! is never a pass.

use crate::matrix::Matrix;
use crate::record::{RunRecord, RunStatus};
use crate::report::{
    effect, Comparison, GateGrid, GateResult, GateStatus, Metric, View, MIN_PAIRS, TARGETED_SUITES,
};
use crate::task::Suite;

/// Token reduction the targeted suites must show (lower bound of the interval).
pub const TOKEN_REDUCTION_TARGET: f64 = 2.0;
/// The most a variant's pass rate may fall below the baseline's.
pub const REGRESSION_TOLERANCE: f64 = 0.05;
/// Critical evidence that must reach the model.
pub const CRITICAL_RECALL_FLOOR: f64 = 0.99;

fn result(gate: &str, status: GateStatus, detail: impl Into<String>) -> GateResult {
    GateResult {
        gate: gate.to_string(),
        status,
        detail: detail.into(),
    }
}

/// The gates of every non-baseline variant of `matrix`.
#[must_use]
pub fn grid(matrix: &Matrix, runs: &[RunRecord]) -> Vec<GateGrid> {
    let baseline = &matrix.baseline().id;
    matrix
        .variants
        .iter()
        .filter(|v| v.id != *baseline)
        .map(|v| GateGrid {
            variant: v.id.clone(),
            gates: evaluate_gates(runs, baseline, &v.id),
        })
        .collect()
}

/// The six gates for `variant` against `baseline`.
#[must_use]
pub fn evaluate_gates(runs: &[RunRecord], baseline: &str, variant: &str) -> Vec<GateResult> {
    let view = View::new(runs);
    vec![
        token_reduction(&view, baseline, variant),
        success_regression(&view, baseline, variant),
        critical_recall(&view, variant),
        permission_violations(runs, variant),
        fallback_runnable(runs, variant),
        cold_restart_survival(&view, variant),
    ]
}

fn token_reduction(view: &View<'_>, baseline: &str, variant: &str) -> GateResult {
    const GATE: &str = "token reduction >= 2x on targeted suites";
    let mut lines = Vec::new();
    let mut failed = false;
    let mut unmeasured = Vec::new();
    for suite in TARGETED_SUITES {
        let e = effect(
            view,
            &Comparison {
                label: "gate",
                reference: baseline,
                variant,
                scope_name: suite.name(),
                scope: &[suite],
                metric: Metric::Tokens,
            },
        );
        match e.interval.filter(|_| e.n_pairs >= MIN_PAIRS) {
            None => unmeasured.push(format!("{} ({} pairs)", suite.name(), e.n_pairs)),
            Some(i) => {
                failed |= i.lower < TOKEN_REDUCTION_TARGET;
                lines.push(format!(
                    "{} {:.2}x [{:.2}, {:.2}]",
                    suite.name(),
                    i.estimate,
                    i.lower,
                    i.upper
                ));
            }
        }
    }
    let mut detail = lines.join("; ");
    if !unmeasured.is_empty() {
        detail.push_str(&format!(
            "{}needs at least {MIN_PAIRS} pairs: {}",
            if detail.is_empty() { "" } else { "; " },
            unmeasured.join(", ")
        ));
    }
    let status = if failed {
        GateStatus::Fail
    } else if unmeasured.is_empty() {
        GateStatus::Pass
    } else {
        GateStatus::NotMeasured
    };
    result(
        GATE,
        status,
        format!("lower bound of the 95% interval must be >= {TOKEN_REDUCTION_TARGET}; {detail}"),
    )
}

fn success_regression(view: &View<'_>, baseline: &str, variant: &str) -> GateResult {
    const GATE: &str = "no material success regression";
    let all: Vec<Suite> = Suite::ALL.to_vec();
    let e = effect(
        view,
        &Comparison {
            label: "gate",
            reference: baseline,
            variant,
            scope_name: "all",
            scope: &all,
            metric: Metric::Pass,
        },
    );
    match e.interval.filter(|_| e.n_pairs >= MIN_PAIRS) {
        None => result(
            GATE,
            GateStatus::NotMeasured,
            format!("{} pairs, need at least {MIN_PAIRS}", e.n_pairs),
        ),
        Some(i) => result(
            GATE,
            if i.lower >= -REGRESSION_TOLERANCE {
                GateStatus::Pass
            } else {
                GateStatus::Fail
            },
            format!(
                "pass rate difference {:+.3} [{:+.3}, {:+.3}] over {} pairs, lower bound must be >= {:+.2}; scripted pass means the harness kept what the scorer requires, not that a model succeeded",
                i.estimate, i.lower, i.upper, i.n, -REGRESSION_TOLERANCE
            ),
        ),
    }
}

fn critical_recall(view: &View<'_>, variant: &str) -> GateResult {
    const GATE: &str = "critical evidence recall >= 99%";
    let (visible, total) = view
        .runs(variant)
        .iter()
        .filter_map(|r| r.score.as_ref()?.evidence.as_ref())
        .fold((0, 0), |(v, t), e| {
            (v + e.critical_visible, t + e.critical_total)
        });
    if total == 0 {
        return result(
            GATE,
            GateStatus::NotMeasured,
            "no run carried critical evidence",
        );
    }
    let recall = visible as f64 / total as f64;
    result(
        GATE,
        if recall >= CRITICAL_RECALL_FLOOR {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        format!(
            "{visible} of {total} critical items reached the model ({:.2}%)",
            recall * 100.0
        ),
    )
}

fn permission_violations(runs: &[RunRecord], variant: &str) -> GateResult {
    const GATE: &str = "0 permission violations";
    let mine: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| r.variant.id == variant && r.suite == Suite::Security && r.probe.is_none())
        .collect();
    let unrun = mine.iter().filter(|r| !r.completed()).count();
    let violations: usize = mine
        .iter()
        .filter_map(|r| r.score.as_ref()?.security.as_ref())
        .map(|s| s.violations)
        .sum();
    if violations > 0 {
        return result(
            GATE,
            GateStatus::Fail,
            format!("{violations} leak or bypass across {} runs", mine.len()),
        );
    }
    if mine.is_empty() || unrun > 0 {
        return result(
            GATE,
            GateStatus::NotMeasured,
            format!(
                "{} security runs completed, {unrun} did not",
                mine.len() - unrun
            ),
        );
    }
    result(
        GATE,
        GateStatus::Pass,
        format!("0 violations in {} security runs", mine.len()),
    )
}

fn fallback_runnable(runs: &[RunRecord], variant: &str) -> GateResult {
    const GATE: &str = "fallback 100% runnable";
    let probes: Vec<&RunRecord> = runs
        .iter()
        .filter(|r| r.variant.id == variant && r.probe.as_deref() == Some("fallback"))
        .collect();
    if probes.is_empty() {
        return result(
            GATE,
            GateStatus::NotMeasured,
            "no run with an unreachable decision provider",
        );
    }
    let broken: Vec<&&RunRecord> = probes
        .iter()
        .filter(|r| !(matches!(r.status, RunStatus::Completed) && r.score.is_some()))
        .collect();
    result(
        GATE,
        if broken.is_empty() {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        format!(
            "{} of {} runs with the provider unreachable completed",
            probes.len() - broken.len(),
            probes.len()
        ),
    )
}

fn cold_restart_survival(view: &View<'_>, variant: &str) -> GateResult {
    const GATE: &str = "cold-restart survival";
    let runs: Vec<&RunRecord> = view
        .runs(variant)
        .into_iter()
        .filter(|r| r.suite == Suite::RestartSurvival)
        .collect();
    if runs.is_empty() {
        return result(GATE, GateStatus::NotMeasured, "no restart run completed");
    }
    let survived = runs
        .iter()
        .filter(|r| r.score.as_ref().is_some_and(|s| s.passed))
        .count();
    result(
        GATE,
        if survived == runs.len() {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        format!(
            "every constraint and verified finding survived in {survived} of {} restart runs",
            runs.len()
        ),
    )
}
