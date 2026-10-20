// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Offline test runner and comparer for sven harnesses.
//!
//! A harness directory holds a main document (`harness.yaml` or
//! `bundle.yaml`), optional `children/` documents and `cases/` manifests.
//! A case names its input, a fixture model, fixture tools and what the run
//! must do; [`run_case`] executes it on a fresh engine and judges it,
//! [`run_suite`] does so for a directory, and [`compare`] runs two variants
//! over one case set.
//!
//! Everything here is mock-only by default. Models come from the scripted,
//! rule-driven or failing mocks; tools are fixtures. A `live` model fixture
//! is parsed but built only under an explicit [`OptIn`] (a flag AND an
//! endpoint); otherwise the case is skipped. The testkit never reads the
//! user's configuration: engines use `Config::default()` and an injected
//! provider.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod case;
mod compare;
mod dir;
mod error;
mod expect;
mod fixture_model;
mod fixture_tool;
mod inspect;
mod paired;
mod render;
mod report;
mod run;
mod runner;
mod source;
mod stats;
mod status;
mod view;

pub use case::{
    Bounds, CaseManifest, Expect, FailureExpect, ResumePhase, ToolFixture, ToolResponse, Write,
};
pub use compare::compare;
pub use dir::HarnessDir;
pub use error::TestkitError;
pub use expect::{includes, json_diff};
pub use fixture_model::{ModelFixture, OptIn, SequenceItem, SequenceToolCall, SequenceUsage};
pub use inspect::{
    inspect, io_schema, main_plan, validate, ChildInfo, HelperInfo, InspectReport, StepInfo,
    ValidateReport, ValidatedHarness,
};
pub use paired::{
    compare_paired, CostComparison, CostTotals, Decision, PairedOptions, PairedReport, TaskPairing,
    Verdict,
};
pub use render::{render_prompts, render_step};
pub use report::{
    CaseComparison, CaseReport, CaseStatus, Change, ComparisonReport, FailureSummary, JsonDiff,
    Mismatch, RunStatus, RunSummary, SuiteReport, VariantSummary,
};
pub use run::{
    replay_once, resume_on_engine, resume_once, run_on_engine, run_once, ActivityFn, CheckpointFn,
    EngineRun, ProgressFn, ReplaySummary, RunRequest, RunResult, Workspace,
};
pub use runner::{run_case, run_suite, RunnerOptions};
pub use stats::{bootstrap_mean, Estimate, CONFIDENCE};
pub use status::{status, CurrentStep, LoopProgress, StatusCounts, StatusReport};
pub use view::{compile, explain_plan, explain_step, graph, CompiledHarness, GraphFormat};
