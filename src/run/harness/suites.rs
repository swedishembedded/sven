// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness validate`, `test` and `compare`: reports over one or more
//! harness directories.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};

use sven_harness_testkit::{
    compare as compare_variants, run_suite, validate as validate_harness, CaseManifest, CaseStatus,
    Change, ComparisonReport, HarnessDir, OptIn, RunnerOptions, SuiteReport, VariantSummary,
};

use super::{kit, print_json, usage, Outcome};

/// Compiles every path; exit 1 when any is invalid, 2 when one is unreadable.
pub(super) fn validate(paths: &[PathBuf], json: bool) -> Outcome {
    if let Some(missing) = paths.iter().find(|p| !p.exists()) {
        return Err(usage(format!("{} does not exist", missing.display())));
    }
    let reports: Vec<_> = paths.iter().map(|p| validate_harness(p)).collect();
    if json {
        print_json(&reports)?;
    } else {
        for report in &reports {
            if report.valid {
                let name = report.harnesses.last().map_or("", |h| h.name.as_str());
                println!("OK {} ({name})", report.path.display());
            } else {
                println!("INVALID {}", report.path.display());
                for error in &report.errors {
                    println!("  {error}");
                }
            }
        }
    }
    Ok(i32::from(reports.iter().any(|r| !r.valid)))
}

/// Runs the cases of every directory. Exit 2 for a directory that is not a
/// valid harness, a case manifest that does not parse or a `--case` that
/// names nothing; otherwise 1 when a case failed.
pub(super) async fn test(
    dirs: &[PathBuf],
    case: Option<&str>,
    replay: bool,
    json: bool,
) -> Outcome {
    let mut opened = Vec::new();
    let mut named = false;
    for path in dirs {
        let dir = HarnessDir::open(path).map_err(kit)?;
        for file in &dir.cases {
            let manifest = CaseManifest::from_path(file).map_err(kit)?;
            named |= case == Some(manifest.name.as_str());
        }
        opened.push(dir);
    }
    if let Some(wanted) = case.filter(|_| !named) {
        return Err(usage(format!("no case named `{wanted}` in any directory")));
    }
    let options = RunnerOptions {
        opt_in: OptIn::from_env(),
        replay_check: replay,
        case_filter: case.map(str::to_string),
    };
    let mut reports = Vec::new();
    for dir in &opened {
        reports.push(run_suite(dir, &options).await);
    }
    if json {
        print_json(&reports)?;
    } else {
        for report in &reports {
            if reports.len() > 1 {
                println!("== {}", report.path.display());
            }
            print_suite(report);
        }
    }
    if let Some(bad) = reports.iter().find(|r| !r.valid) {
        eprintln!(
            "error: {} is not a valid harness: {}",
            bad.path.display(),
            bad.error.as_deref().unwrap_or("unknown")
        );
        return Ok(2);
    }
    Ok(i32::from(reports.iter().any(|r| r.failed > 0)))
}

fn print_suite(report: &SuiteReport) {
    if !report.valid {
        println!(
            "INVALID {}: {}",
            report.path.display(),
            report.error.as_deref().unwrap_or("unknown")
        );
        return;
    }
    for case in &report.cases {
        match &case.status {
            CaseStatus::Passed => println!("PASS {}", case.case),
            CaseStatus::Failed => {
                println!("FAIL {}", case.case);
                for m in &case.mismatches {
                    println!(
                        "  {}: expected {}, got {}",
                        m.expectation, m.expected, m.actual
                    );
                }
            }
            CaseStatus::Skipped { reason } => println!("SKIP {} ({reason})", case.case),
            CaseStatus::Error { message } => println!("ERROR {}: {message}", case.case),
        }
    }
    let skipped = if report.skipped > 0 {
        format!(", {} skipped", report.skipped)
    } else {
        String::new()
    };
    println!(
        "{} passed, {} failed{skipped}",
        report.passed, report.failed
    );
}

/// Runs one case set over two variants. Exit 2 for an invalid variant; 1
/// for a regression under `--fail-on-regression`.
pub(super) async fn compare(
    dir_a: &Path,
    dir_b: &Path,
    cases: &Path,
    fail_on_regression: bool,
    json: bool,
) -> Outcome {
    let options = RunnerOptions {
        opt_in: OptIn::from_env(),
        ..RunnerOptions::default()
    };
    let report = compare_variants(dir_a, dir_b, cases, &options)
        .await
        .map_err(kit)?;
    if json {
        print_json(&report)?;
    } else {
        print_comparison(&report);
    }
    if let Some(bad) = [&report.a, &report.b].into_iter().find(|v| !v.valid) {
        eprintln!(
            "error: {} is not a valid harness: {}",
            bad.path.display(),
            bad.error.as_deref().unwrap_or("unknown")
        );
        return Ok(2);
    }
    let regressed = report.cases.iter().any(|c| c.change == Change::Regressed);
    Ok(i32::from(fail_on_regression && regressed))
}

fn print_comparison(report: &ComparisonReport) {
    for (label, v) in [("A", &report.a), ("B", &report.b)] {
        print_variant(label, v);
    }
    for case in &report.cases {
        let change = match case.change {
            Change::None => "same",
            Change::Regressed => "regressed",
            Change::Fixed => "fixed",
        };
        println!("{}: {change}", case.case);
        for diff in &case.output_diff {
            println!(
                "  {}: A {:?}, B {:?}",
                diff.pointer, diff.expected, diff.actual
            );
        }
    }
}

fn print_variant(label: &str, v: &VariantSummary) {
    if !v.valid {
        println!(
            "{label} {}: INVALID {}",
            v.path.display(),
            v.error.as_deref().unwrap_or("")
        );
        return;
    }
    println!(
        "{label} {}: {} passed, {} failed, {} skipped; {} steps; tokens in {:?} out {:?}",
        v.path.display(),
        v.passed,
        v.failed,
        v.skipped,
        v.steps,
        v.input_tokens,
        v.output_tokens
    );
}
