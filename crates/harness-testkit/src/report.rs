// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The report shapes: every one serializes to JSON so a CLI or CI job can
//! print or archive it unchanged.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a harness run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    /// The plan reached a `return`.
    Completed,
    /// The plan reached a `fail`, or a limit the plan declared ran out.
    Failed,
    /// The caller cancelled the run.
    Cancelled,
    /// A bound (deadline or token budget) stopped the run before a verdict;
    /// it left a checkpoint.
    Suspended,
}

/// A plan failure: the code and message a `fail` step reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureSummary {
    /// The failure code.
    pub code: String,
    /// The failure message.
    pub message: String,
}

/// What one run did, in JSON-ready form.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunSummary {
    /// How the run ended.
    pub status: RunStatus,
    /// The returned value of a completed run.
    pub output: Option<Value>,
    /// The failure of a failed or cancelled run.
    pub failure: Option<FailureSummary>,
    /// Step executions.
    pub steps: u64,
    /// Start counts by step id (top-level run only).
    pub step_counts: BTreeMap<String, u64>,
    /// Names of the fixture tools that ran, sorted, children included.
    pub tools_invoked: Vec<String>,
    /// Prompt tokens the provider reported, when it reported any.
    pub input_tokens: Option<u64>,
    /// Generated tokens the provider reported, when it reported any.
    pub output_tokens: Option<u64>,
    /// The plan's digest.
    pub plan_digest: String,
}

/// One difference between an expected and an actual JSON value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JsonDiff {
    /// RFC 6901 pointer to the differing position.
    pub pointer: String,
    /// The expected value; `None` where nothing was expected.
    pub expected: Option<Value>,
    /// The actual value; `None` where nothing was found.
    pub actual: Option<Value>,
}

/// One expectation a run did not meet.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mismatch {
    /// The expectation's name, e.g. `status`, `steps.ask`, `output`.
    pub expectation: String,
    /// What was expected.
    pub expected: Value,
    /// What happened.
    pub actual: Value,
    /// For structured values, where they part.
    pub diff: Vec<JsonDiff>,
}

/// The verdict on a case.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum CaseStatus {
    /// Every expectation held.
    Passed,
    /// At least one expectation did not.
    Failed,
    /// The case was not run.
    Skipped {
        /// Why.
        reason: String,
    },
    /// The case could not be judged (bad manifest, exhausted fixture, ...).
    Error {
        /// What went wrong.
        message: String,
    },
}

/// The report on one case.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseReport {
    /// The case's name.
    pub case: String,
    /// The verdict.
    #[serde(flatten)]
    pub status: CaseStatus,
    /// The (last phase's) run, when one happened.
    pub run: Option<RunSummary>,
    /// Whether every run of the case was replayed offline and matched.
    pub replay_checked: bool,
    /// The expectations that did not hold.
    pub mismatches: Vec<Mismatch>,
    /// Wall-clock time of the case.
    pub duration_ms: u64,
}

/// The report on a harness directory.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuiteReport {
    /// The harness's name, when it loaded.
    pub harness: String,
    /// The path that was run.
    pub path: PathBuf,
    /// Whether the harness compiled.
    pub valid: bool,
    /// Why it did not.
    pub error: Option<String>,
    /// Cases passed.
    pub passed: usize,
    /// Cases failed or errored.
    pub failed: usize,
    /// Cases skipped.
    pub skipped: usize,
    /// Each case's report, in file order.
    pub cases: Vec<CaseReport>,
}

/// How a case's verdict moved between two variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    /// No move.
    None,
    /// It passed in A and does not in B.
    Regressed,
    /// It did not pass in A and does in B.
    Fixed,
}

/// One case across two variants.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaseComparison {
    /// The case's name.
    pub case: String,
    /// A's report (`None` when A could not run it).
    pub a: Option<CaseReport>,
    /// B's report (`None` when B could not run it).
    pub b: Option<CaseReport>,
    /// Where the two outputs part (A expected, B actual).
    pub output_diff: Vec<JsonDiff>,
    /// Whether the verdicts differ.
    pub status_changed: bool,
    /// Which way.
    pub change: Change,
}

/// One variant's totals over the case set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VariantSummary {
    /// The variant's path.
    pub path: PathBuf,
    /// Whether it compiled.
    pub valid: bool,
    /// Why it did not.
    pub error: Option<String>,
    /// Its plan digest, when valid.
    pub plan_digest: Option<String>,
    /// Cases passed.
    pub passed: usize,
    /// Cases failed or errored.
    pub failed: usize,
    /// Cases skipped.
    pub skipped: usize,
    /// Prompt tokens reported across the cases, when any were.
    pub input_tokens: Option<u64>,
    /// Generated tokens reported across the cases, when any were.
    pub output_tokens: Option<u64>,
    /// Wall-clock time across the cases.
    pub duration_ms: u64,
    /// Step executions across the cases.
    pub steps: u64,
}

/// Two variants over one case set.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ComparisonReport {
    /// Variant A.
    pub a: VariantSummary,
    /// Variant B.
    pub b: VariantSummary,
    /// Each case, in file order.
    pub cases: Vec<CaseComparison>,
}
