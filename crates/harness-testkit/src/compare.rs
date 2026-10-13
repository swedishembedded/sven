// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compare two harness variants over one case set.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in comparing
//! agent harness revisions then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::path::Path;

use serde_json::Value;

use crate::dir::{yaml_files, HarnessDir};
use crate::error::TestkitError;
use crate::expect::json_diff;
use crate::inspect::validate_dir;
use crate::report::{
    CaseComparison, CaseReport, CaseStatus, Change, ComparisonReport, SuiteReport, VariantSummary,
};
use crate::runner::{case_name, run_suite, RunnerOptions};

/// Runs the cases in `cases` against the variants `a` and `b` and reports
/// how each case moved.
///
/// A case that passed in `a` and does not pass in `b` (including because
/// `b` could not be loaded) is a regression; the reverse is a fix. Outputs
/// are diffed with `a` as the expectation.
///
/// # Errors
///
/// [`TestkitError`] when a variant path or the case directory does not
/// exist. A variant that does not compile is reported, not an error.
pub async fn compare(
    a: &Path,
    b: &Path,
    cases: &Path,
    options: &RunnerOptions,
) -> Result<ComparisonReport, TestkitError> {
    for path in [a, b, cases] {
        std::fs::metadata(path).map_err(|source| TestkitError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let names: Vec<String> = yaml_files(cases)?.iter().map(|p| case_name(p)).collect();
    let (summary_a, suite_a) = run_variant(a, cases, options).await;
    let (summary_b, suite_b) = run_variant(b, cases, options).await;
    let cases = names
        .into_iter()
        .map(|name| {
            let find = |suite: &Option<SuiteReport>| {
                suite
                    .as_ref()
                    .and_then(|s| s.cases.iter().find(|c| c.case == name))
                    .cloned()
            };
            compared(name.clone(), find(&suite_a), find(&suite_b))
        })
        .collect();
    Ok(ComparisonReport {
        a: summary_a,
        b: summary_b,
        cases,
    })
}

/// One variant's totals, and its suite when it ran.
async fn run_variant(
    path: &Path,
    cases: &Path,
    options: &RunnerOptions,
) -> (VariantSummary, Option<SuiteReport>) {
    let mut summary = VariantSummary {
        path: path.to_path_buf(),
        valid: false,
        error: None,
        plan_digest: None,
        passed: 0,
        failed: 0,
        skipped: 0,
        input_tokens: None,
        output_tokens: None,
        duration_ms: 0,
        steps: 0,
    };
    let dir = match HarnessDir::open(path).and_then(|dir| dir.with_cases_from(cases)) {
        Ok(dir) => dir,
        Err(e) => {
            summary.error = Some(e.to_string());
            return (summary, None);
        }
    };
    let validation = validate_dir(&dir, path);
    if !validation.valid {
        summary.error = Some(validation.errors.join("; "));
        return (summary, None);
    }
    summary.valid = true;
    summary.plan_digest = validation.harnesses.last().map(|h| h.plan_digest.clone());
    let suite = run_suite(&dir, options).await;
    (summary.passed, summary.failed, summary.skipped) = (suite.passed, suite.failed, suite.skipped);
    for case in &suite.cases {
        summary.duration_ms += case.duration_ms;
        if let Some(run) = &case.run {
            summary.steps += run.steps;
            summary.input_tokens = add_tokens(summary.input_tokens, run.input_tokens);
            summary.output_tokens = add_tokens(summary.output_tokens, run.output_tokens);
        }
    }
    (summary, Some(suite))
}

fn add_tokens(total: Option<u64>, more: Option<u64>) -> Option<u64> {
    match (total, more) {
        (None, None) => None,
        (t, m) => Some(t.unwrap_or(0) + m.unwrap_or(0)),
    }
}

/// How a variant stood on a case.
#[derive(PartialEq)]
enum Stood {
    Passed,
    NotPassed,
    Skipped,
    Absent,
}

fn stood(report: &Option<CaseReport>) -> Stood {
    match report.as_ref().map(|r| &r.status) {
        Some(CaseStatus::Passed) => Stood::Passed,
        Some(CaseStatus::Failed | CaseStatus::Error { .. }) => Stood::NotPassed,
        Some(CaseStatus::Skipped { .. }) => Stood::Skipped,
        None => Stood::Absent,
    }
}

fn compared(case: String, a: Option<CaseReport>, b: Option<CaseReport>) -> CaseComparison {
    let (before, after) = (stood(&a), stood(&b));
    let change = if before == Stood::Passed && matches!(after, Stood::NotPassed | Stood::Absent) {
        Change::Regressed
    } else if after == Stood::Passed && matches!(before, Stood::NotPassed | Stood::Absent) {
        Change::Fixed
    } else {
        Change::None
    };
    let output = |report: &Option<CaseReport>| {
        report
            .as_ref()
            .and_then(|r| r.run.as_ref())
            .map(|run| run.output.clone().unwrap_or(Value::Null))
    };
    let output_diff = match (output(&a), output(&b)) {
        (Some(want), Some(got)) => json_diff(&want, &got),
        _ => Vec::new(),
    };
    CaseComparison {
        case,
        a,
        b,
        output_diff,
        status_changed: before != after,
        change,
    }
}
