// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The case and suite runner.
//!
//! A case runs on a new engine (see [`crate::run_once`]), is judged against
//! its expectations, and - unless turned off - its recording is replayed
//! offline and must reproduce the same verdict and trace. A `resume` phase
//! continues the first phase's checkpoint on the same workspace.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use serde_json::json;

use crate::case::{CaseManifest, ToolFixture};
use crate::dir::HarnessDir;
use crate::error::TestkitError;
use crate::expect;
use crate::fixture_model::{ModelFixture, OptIn};
use crate::inspect::validate_dir;
use crate::report::{CaseReport, CaseStatus, Mismatch, RunSummary, SuiteReport};
use crate::run::{
    replay_once, resume_once, run_once, seed_workspace, RunRequest, RunResult, Workspace,
};

/// How a suite is run.
#[derive(Debug, Clone)]
pub struct RunnerOptions {
    /// Permission to build live models.
    pub opt_in: OptIn,
    /// Replay every recorded run offline and require the same verdict and
    /// trace (a case may still turn it off with `replay: false`).
    pub replay_check: bool,
    /// Run only the case with this name.
    pub case_filter: Option<String>,
    /// The seed every run in the suite uses (see [`RunRequest::seed`]).
    pub seed: Option<u64>,
}

impl Default for RunnerOptions {
    fn default() -> Self {
        Self {
            opt_in: OptIn::none(),
            replay_check: true,
            case_filter: None,
            seed: None,
        }
    }
}

/// How one case ended, before it is dressed as a report.
enum Ended {
    Judged(Box<Judgement>),
    Skipped(String),
    Error(String),
}

/// A case that ran: what it missed and what it left behind.
struct Judgement {
    mismatches: Vec<Mismatch>,
    /// Fixture trouble that makes the verdict untrustworthy.
    problems: Vec<String>,
    run: Option<RunSummary>,
    replayed: bool,
}

impl Judgement {
    fn ended(
        mismatches: Vec<Mismatch>,
        problems: Vec<String>,
        run: RunSummary,
        replayed: bool,
    ) -> Ended {
        Ended::Judged(Box::new(Self {
            mismatches,
            problems,
            run: Some(run),
            replayed,
        }))
    }
}

/// Runs one case file.
pub async fn run_case(dir: &HarnessDir, case_path: &Path, options: &RunnerOptions) -> CaseReport {
    let started = Instant::now();
    let fallback = case_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (name, ended) = match CaseManifest::from_path(case_path) {
        Err(e) => (fallback, Ended::Error(e.to_string())),
        Ok(case) => {
            let ended = execute(dir, &case, options).await;
            (case.name, ended)
        }
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let report = |status, run, replay_checked, mismatches| CaseReport {
        case: name.clone(),
        status,
        run,
        replay_checked,
        mismatches,
        duration_ms,
    };
    match ended {
        Ended::Skipped(reason) => report(CaseStatus::Skipped { reason }, None, false, Vec::new()),
        Ended::Error(message) => report(CaseStatus::Error { message }, None, false, Vec::new()),
        Ended::Judged(j) if !j.problems.is_empty() => report(
            CaseStatus::Error {
                message: j.problems.join("; "),
            },
            j.run,
            j.replayed,
            Vec::new(),
        ),
        Ended::Judged(j) => {
            let status = if j.mismatches.is_empty() {
                CaseStatus::Passed
            } else {
                CaseStatus::Failed
            };
            report(status, j.run, j.replayed, j.mismatches)
        }
    }
}

async fn execute(dir: &HarnessDir, case: &CaseManifest, options: &RunnerOptions) -> Ended {
    let phases = std::iter::once(&case.model).chain(case.resume.iter().map(|r| &r.model));
    for model in phases.flatten() {
        if let Err(reason) = model.live_gate(&options.opt_in) {
            return Ended::Skipped(reason);
        }
    }
    let workspace = match tempfile::tempdir() {
        Ok(tmp) => tmp,
        Err(e) => return Ended::Error(format!("cannot create a workspace: {e}")),
    };
    if let Err(e) = seed_workspace(workspace.path(), &case.workspace) {
        return Ended::Error(e.to_string());
    }
    // The case's runs (and its resumes) share one private directory, removed
    // with the case rather than left in the temporary directory.
    let scratchpad = match tempfile::tempdir() {
        Ok(tmp) => tmp,
        Err(e) => return Ended::Error(format!("cannot create a scratchpad: {e}")),
    };
    let replay_on = case.replay && options.replay_check;
    let request =
        |model: &Option<ModelFixture>, tools: &BTreeMap<String, ToolFixture>| RunRequest {
            input: case.input.clone(),
            model: model.clone(),
            tools: tools.clone(),
            workspace: Workspace::Dir(workspace.path().to_path_buf()),
            opt_in: options.opt_in.clone(),
            seed: options.seed,
            record: replay_on,
            scratchpad: Some(scratchpad.path().to_path_buf()),
            ..RunRequest::default()
        };

    let mut first = request(&case.model, &case.tools);
    first.bounds = case.bounds.clone();
    let first = match run_once(dir, first).await {
        Ok(result) => result,
        Err(e) => return failed_to_run(e),
    };
    let mut mismatches = judge(dir, &case.expect, &first, replay_on, "");
    let mut problems = first.fixtures.problems.clone();
    let mut last = first;

    if let Some(resume) = &case.resume {
        let Some(checkpoint) = last.checkpoint.clone() else {
            mismatches.push(mismatch(
                "resume",
                json!("a checkpoint to resume"),
                json!("none: the first phase left no checkpoint"),
            ));
            return Judgement::ended(mismatches, problems, last.summary, replay_on);
        };
        let second = request(&resume.model, &resume.tools);
        match resume_once(dir, checkpoint, second).await {
            Ok(result) => {
                mismatches.extend(judge(dir, &resume.expect, &result, replay_on, "resume."));
                problems.extend(result.fixtures.problems.clone());
                last = result;
            }
            Err(e) => return failed_to_run(e),
        }
    }
    Judgement::ended(mismatches, problems, last.summary, replay_on)
}

fn failed_to_run(error: TestkitError) -> Ended {
    match error {
        TestkitError::LiveNotOptedIn(reason) => Ended::Skipped(reason),
        other => Ended::Error(other.to_string()),
    }
}

fn mismatch(expectation: &str, expected: serde_json::Value, actual: serde_json::Value) -> Mismatch {
    Mismatch {
        expectation: expectation.to_string(),
        expected,
        actual,
        diff: Vec::new(),
    }
}

/// The expectations a phase's run missed: its own, the leak check, and the
/// offline replay.
fn judge(
    dir: &HarnessDir,
    expect: &crate::case::Expect,
    result: &RunResult,
    replay: bool,
    prefix: &str,
) -> Vec<Mismatch> {
    let mut found = expect::check(expect, &result.summary, &result.fixtures, prefix);
    if result.fixtures.leaked > 0 {
        found.push(mismatch(
            &format!("{prefix}leaked tool call"),
            json!(0),
            json!(result.fixtures.leaked),
        ));
    }
    if replay {
        if let Some(found_replay) = replay_mismatch(dir, result, prefix) {
            found.push(found_replay);
        }
    }
    found
}

/// A mismatch when replaying the run's recording does not reproduce its
/// verdict and trace.
fn replay_mismatch(dir: &HarnessDir, result: &RunResult, prefix: &str) -> Option<Mismatch> {
    let name = format!("{prefix}replay");
    let Some(recording) = &result.recording else {
        return Some(mismatch(&name, json!("a recording"), json!("none")));
    };
    let replayed = match replay_once(dir, recording) {
        Ok(replayed) => replayed,
        Err(e) => return Some(mismatch(&name, json!("a replay"), json!(e.to_string()))),
    };
    let same = replayed.output == result.summary.output
        && replayed.failure == result.summary.failure
        && replayed.trace == result.outcome.trace;
    (!same).then(|| {
        mismatch(
            &name,
            json!({"output": result.summary.output, "failure": result.summary.failure, "trace_records": result.outcome.trace.len()}),
            json!({"output": replayed.output, "failure": replayed.failure, "trace_records": replayed.trace.len()}),
        )
    })
}

/// Runs every case of a harness directory.
pub async fn run_suite(dir: &HarnessDir, options: &RunnerOptions) -> SuiteReport {
    let validation = validate_dir(dir, &dir.root);
    let harness = validation
        .harnesses
        .last()
        .map(|h| h.name.clone())
        .unwrap_or_default();
    let mut report = SuiteReport {
        harness,
        path: dir.root.clone(),
        valid: validation.valid,
        error: (!validation.valid).then(|| validation.errors.join("; ")),
        passed: 0,
        failed: 0,
        skipped: 0,
        cases: Vec::new(),
    };
    if !validation.valid {
        return report;
    }
    for path in &dir.cases {
        if options
            .case_filter
            .as_ref()
            .is_some_and(|wanted| *wanted != case_name(path))
        {
            continue;
        }
        let case = run_case(dir, path, options).await;
        match case.status {
            CaseStatus::Passed => report.passed += 1,
            CaseStatus::Failed | CaseStatus::Error { .. } => report.failed += 1,
            CaseStatus::Skipped { .. } => report.skipped += 1,
        }
        report.cases.push(case);
    }
    report
}

/// A case's name without running it: its manifest's, else its file stem.
pub(crate) fn case_name(path: &Path) -> String {
    CaseManifest::from_path(path)
        .map(|c| c.name)
        .unwrap_or_else(|_| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}
