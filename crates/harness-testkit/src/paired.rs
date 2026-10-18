// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Paired comparison of two harness variants.
//!
//! Both variants run every task under every seed, so each (task, seed) pair
//! is a matched observation. Tasks, not runs, are the unit of inference:
//! seeds of one task are not independent evidence about the harness, so the
//! interval resamples per-task pass-rate differences. A candidate is
//! rejected when any task passes in fewer seeds than under the baseline (a
//! protected task must not get worse), promoted only when no task regressed
//! and the interval on the mean change excludes zero, and otherwise left
//! inconclusive. Cost is totalled per variant and compared per task.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in comparing
//! agent harness revisions then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::compare::run_variant;
use crate::dir::yaml_files;
use crate::error::TestkitError;
use crate::report::{CaseReport, CaseStatus, Change, SuiteReport, VariantSummary};
use crate::runner::{case_name, RunnerOptions};
use crate::stats::{bootstrap_mean, Estimate, CONFIDENCE};

/// How a paired comparison is run.
#[derive(Debug, Clone)]
pub struct PairedOptions {
    /// The seeds both variants run every task under, in report order.
    pub seeds: Vec<u64>,
    /// Seeds the bootstrap; the same value gives the same intervals.
    pub bootstrap_seed: u64,
    /// Bootstrap resamples.
    pub resamples: usize,
}

/// One task across the seeds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TaskPairing {
    /// The task (case) name.
    pub task: String,
    /// Whether the baseline passed under each seed, in seed order.
    pub passed_a: Vec<bool>,
    /// Whether the candidate passed under each seed, in seed order.
    pub passed_b: Vec<bool>,
    /// Candidate pass rate minus baseline pass rate.
    pub delta: f64,
    /// Which way the pass rate moved.
    pub change: Change,
    /// Candidate minus baseline mean output tokens per seed, when both
    /// reported usage.
    pub output_token_delta: Option<f64>,
}

/// What one variant spent over every (task, seed) run.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CostTotals {
    /// Runs that happened.
    pub runs: u64,
    /// Prompt tokens the provider reported.
    pub input_tokens: u64,
    /// Generated tokens the provider reported.
    pub output_tokens: u64,
    /// Step executions.
    pub steps: u64,
    /// Wall-clock milliseconds.
    pub duration_ms: u64,
}

/// Cost of both variants, and the paired output-token change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostComparison {
    /// The baseline's totals.
    pub a: CostTotals,
    /// The candidate's totals.
    pub b: CostTotals,
    /// Mean per-task change in output tokens per seed (candidate minus
    /// baseline), with its interval; `None` when no task reported usage.
    pub output_tokens_delta: Option<Estimate>,
}

/// The verdict on the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// No task regressed and the mean change is positive beyond the interval.
    Promote,
    /// A task regressed, or the candidate does not run.
    Reject,
    /// Nothing regressed, but the evidence does not show an improvement.
    Inconclusive,
}

/// A verdict and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    /// The verdict.
    pub verdict: Verdict,
    /// The reasons, one per line of the explanation.
    pub reasons: Vec<String>,
}

/// A paired comparison of a baseline (`a`) and a candidate (`b`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PairedReport {
    /// The baseline.
    pub a: VariantSummary,
    /// The candidate.
    pub b: VariantSummary,
    /// The seeds each task ran under.
    pub seeds: Vec<u64>,
    /// The confidence level of the intervals.
    pub confidence: f64,
    /// Each compared task, in file order.
    pub tasks: Vec<TaskPairing>,
    /// Tasks left out because a variant skipped them under some seed.
    pub skipped: Vec<String>,
    /// Mean change in task pass rate with its interval.
    pub mean_delta: Estimate,
    /// Tasks whose pass rate fell.
    pub regressions: Vec<String>,
    /// Cost of both variants.
    pub cost: CostComparison,
    /// The verdict.
    pub decision: Decision,
}

/// Per-seed observation of one variant.
struct Observed {
    summary: VariantSummary,
    suites: Vec<Option<SuiteReport>>,
}

async fn observe(path: &Path, cases: &Path, options: &RunnerOptions, seeds: &[u64]) -> Observed {
    let mut summary: Option<VariantSummary> = None;
    let mut suites = Vec::new();
    for seed in seeds {
        let seeded = RunnerOptions {
            seed: Some(*seed),
            ..options.clone()
        };
        let (one, suite) = run_variant(path, cases, &seeded).await;
        summary = Some(match summary {
            None => one,
            Some(mut total) => {
                total.passed += one.passed;
                total.failed += one.failed;
                total.skipped += one.skipped;
                total.steps += one.steps;
                total.duration_ms += one.duration_ms;
                total.input_tokens = sum_opt(total.input_tokens, one.input_tokens);
                total.output_tokens = sum_opt(total.output_tokens, one.output_tokens);
                total
            }
        });
        suites.push(suite);
    }
    Observed {
        summary: summary.expect("at least one seed, checked by the caller"),
        suites,
    }
}

fn sum_opt(total: Option<u64>, more: Option<u64>) -> Option<u64> {
    match (total, more) {
        (None, None) => None,
        (t, m) => Some(t.unwrap_or(0) + m.unwrap_or(0)),
    }
}

/// The case's report under each seed; `None` where the variant could not run
/// it (an invalid variant or a missing case).
fn reports_of<'a>(observed: &'a Observed, task: &str) -> Vec<Option<&'a CaseReport>> {
    observed
        .suites
        .iter()
        .map(|suite| {
            suite
                .as_ref()
                .and_then(|s| s.cases.iter().find(|c| c.case == task))
        })
        .collect()
}

fn skipped(report: Option<&CaseReport>) -> bool {
    matches!(report.map(|r| &r.status), Some(CaseStatus::Skipped { .. }))
}

fn passed(report: Option<&CaseReport>) -> bool {
    matches!(report.map(|r| &r.status), Some(CaseStatus::Passed))
}

fn rate(passes: &[bool]) -> f64 {
    passes.iter().filter(|p| **p).count() as f64 / passes.len() as f64
}

fn mean_output_tokens(reports: &[Option<&CaseReport>]) -> Option<f64> {
    let tokens: Vec<u64> = reports
        .iter()
        .filter_map(|r| {
            r.and_then(|r| r.run.as_ref())
                .and_then(|run| run.output_tokens)
        })
        .collect();
    (tokens.len() == reports.len()).then(|| tokens.iter().sum::<u64>() as f64 / tokens.len() as f64)
}

fn totals(observed: &Observed) -> CostTotals {
    let mut totals = CostTotals::default();
    for case in observed.suites.iter().flatten().flat_map(|s| &s.cases) {
        totals.runs += 1;
        totals.duration_ms += case.duration_ms;
        if let Some(run) = &case.run {
            totals.steps += run.steps;
            totals.input_tokens += run.input_tokens.unwrap_or(0);
            totals.output_tokens += run.output_tokens.unwrap_or(0);
        }
    }
    totals
}

/// Runs every case in `cases` under every seed of `paired` against the
/// baseline `a` and the candidate `b` and reports the matched outcomes.
///
/// # Errors
///
/// [`TestkitError`] when a path does not exist, no seed is given, or no task
/// is comparable. A variant that does not compile is reported (and rejected),
/// not an error.
pub async fn compare_paired(
    a: &Path,
    b: &Path,
    cases: &Path,
    options: &RunnerOptions,
    paired: &PairedOptions,
) -> Result<PairedReport, TestkitError> {
    for path in [a, b, cases] {
        std::fs::metadata(path).map_err(|source| TestkitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    if paired.seeds.is_empty() {
        return Err(TestkitError::Run(
            "a paired comparison needs at least one seed".into(),
        ));
    }
    let unique: BTreeSet<_> = paired.seeds.iter().collect();
    if unique.len() != paired.seeds.len() {
        return Err(TestkitError::Run(
            "the seeds of a paired comparison must be distinct".into(),
        ));
    }
    let (observed_a, observed_b) = (
        observe(a, cases, options, &paired.seeds).await,
        observe(b, cases, options, &paired.seeds).await,
    );

    let mut tasks = Vec::new();
    let mut left_out = Vec::new();
    for path in yaml_files(cases)? {
        let task = case_name(&path);
        let (reports_a, reports_b) = (
            reports_of(&observed_a, &task),
            reports_of(&observed_b, &task),
        );
        if reports_a.iter().chain(&reports_b).any(|r| skipped(*r)) {
            left_out.push(task);
            continue;
        }
        let passed_a: Vec<bool> = reports_a.iter().map(|r| passed(*r)).collect();
        let passed_b: Vec<bool> = reports_b.iter().map(|r| passed(*r)).collect();
        let delta = rate(&passed_b) - rate(&passed_a);
        let change = if delta < 0.0 {
            Change::Regressed
        } else if delta > 0.0 {
            Change::Fixed
        } else {
            Change::None
        };
        let output_token_delta = mean_output_tokens(&reports_a)
            .zip(mean_output_tokens(&reports_b))
            .map(|(before, after)| after - before);
        tasks.push(TaskPairing {
            task,
            passed_a,
            passed_b,
            delta,
            change,
            output_token_delta,
        });
    }
    let deltas: Vec<f64> = tasks.iter().map(|t| t.delta).collect();
    let mean_delta = bootstrap_mean(&deltas, paired.bootstrap_seed, paired.resamples)
        .ok_or_else(|| TestkitError::Run("no task is comparable under every seed".into()))?;
    let token_deltas: Vec<f64> = tasks.iter().filter_map(|t| t.output_token_delta).collect();
    let cost = CostComparison {
        a: totals(&observed_a),
        b: totals(&observed_b),
        output_tokens_delta: (token_deltas.len() == tasks.len())
            .then(|| bootstrap_mean(&token_deltas, paired.bootstrap_seed, paired.resamples))
            .flatten(),
    };
    let regressions: Vec<String> = tasks
        .iter()
        .filter(|t| t.change == Change::Regressed)
        .map(|t| t.task.clone())
        .collect();
    let decision = decide(
        &observed_a.summary,
        &observed_b.summary,
        &regressions,
        &mean_delta,
    );
    Ok(PairedReport {
        a: observed_a.summary,
        b: observed_b.summary,
        seeds: paired.seeds.clone(),
        confidence: CONFIDENCE,
        tasks,
        skipped: left_out,
        mean_delta,
        regressions,
        cost,
        decision,
    })
}

fn decide(
    a: &VariantSummary,
    b: &VariantSummary,
    regressions: &[String],
    delta: &Estimate,
) -> Decision {
    let decision = |verdict, reason: String| Decision {
        verdict,
        reasons: vec![reason],
    };
    if !b.valid {
        return decision(
            Verdict::Reject,
            format!(
                "the candidate does not compile: {}",
                b.error.as_deref().unwrap_or("unknown")
            ),
        );
    }
    if !a.valid {
        return decision(
            Verdict::Inconclusive,
            format!(
                "the baseline does not compile: {}",
                a.error.as_deref().unwrap_or("unknown")
            ),
        );
    }
    if !regressions.is_empty() {
        return decision(
            Verdict::Reject,
            format!("pass rate fell on: {}", regressions.join(", ")),
        );
    }
    if delta.lo > 0.0 {
        decision(
            Verdict::Promote,
            format!(
                "mean pass-rate change {:+.3}, {:.0}% interval [{:+.3}, {:+.3}] excludes zero; no task regressed",
                delta.value,
                CONFIDENCE * 100.0,
                delta.lo,
                delta.hi
            ),
        )
    } else {
        decision(
            Verdict::Inconclusive,
            format!(
                "no task regressed, but the {:.0}% interval [{:+.3}, {:+.3}] on the mean change includes zero",
                CONFIDENCE * 100.0,
                delta.lo,
                delta.hi
            ),
        )
    }
}
