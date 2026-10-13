// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Single runs: start a harness on a fresh engine with fixture models and
//! tools, resume a checkpoint, or replay a recording - no case manifest
//! involved. The runner and the CLI are both built on these.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use sven_model_mock::FailingMockProvider;
use sven_sdk::config::Config;
use sven_sdk::{
    ApprovalPolicy, CancelToken, Engine, Harness, HarnessCheckpoint, HarnessFailure,
    HarnessOutcome, HarnessRecording, HarnessRun, RunConclusion, RunOptions, Toolset,
};

use crate::case::{Bounds, ToolFixture};
use crate::dir::{jail_join, HarnessDir};
use crate::error::TestkitError;
use crate::fixture_model::{self, BuiltModel, ModelFixture, OptIn};
use crate::fixture_tool::{FixtureTool, ToolBooks};
use crate::report::{FailureSummary, RunStatus, RunSummary};
use crate::source::Source;

/// Where a run's project root comes from.
#[derive(Debug, Clone)]
pub enum Workspace {
    /// A fresh tempdir seeded with these files (relative path to text),
    /// removed when the run ends.
    Fresh(BTreeMap<String, String>),
    /// An existing directory, used as the project root as it is.
    Dir(PathBuf),
}

impl Default for Workspace {
    fn default() -> Self {
        Self::Fresh(BTreeMap::new())
    }
}

/// Everything a single run needs besides the harness.
#[derive(Debug, Clone, Default)]
pub struct RunRequest {
    /// The run's inputs (checked against the harness's declared inputs).
    pub input: Value,
    /// The fixture model; `None` is a model that only fails.
    pub model: Option<ModelFixture>,
    /// Fixture tools by name.
    pub tools: BTreeMap<String, ToolFixture>,
    /// The project root.
    pub workspace: Workspace,
    /// Deadline, token budget and cancellation timer.
    pub bounds: Bounds,
    /// Permission to build a live model.
    pub opt_in: OptIn,
    /// Keep a recording of the machine's input events.
    pub record: bool,
    /// Build an ATIF trajectory after the run.
    pub trajectory: bool,
}

/// What a single run produced.
#[derive(Debug)]
pub struct RunResult {
    /// The JSON-ready summary.
    pub summary: RunSummary,
    /// The SDK's own outcome (full trace, usage).
    pub outcome: HarnessOutcome,
    /// The checkpoint a bound left.
    pub checkpoint: Option<HarnessCheckpoint>,
    /// The recording, when `record` was set.
    pub recording: Option<HarnessRecording>,
    /// The ATIF trajectory, when `trajectory` was set.
    pub trajectory: Option<Value>,
    /// The run's id.
    pub run_id: String,
    /// The bundle digest, when the harness is a bundle.
    pub bundle_digest: Option<String>,
    /// Wall-clock time.
    pub duration_ms: u64,
    /// Fixture-side facts the verdict needs.
    pub(crate) fixtures: FixtureFacts,
}

/// What the fixtures saw during a run.
#[derive(Debug, Clone, Default)]
pub(crate) struct FixtureFacts {
    /// Capabilities of executed fixture calls.
    pub capabilities: Vec<sven_sdk::tool::ToolCapability>,
    /// Fixture tool calls still in flight after the run ended.
    pub leaked: usize,
    /// Exhausted queues, refused writes, exhausted model sequences.
    pub problems: Vec<String>,
}

/// What an offline replay reproduced.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReplaySummary {
    /// The machine's final state label.
    pub state: String,
    /// The returned value, when the replay reached a `return`.
    pub output: Option<Value>,
    /// The failure, when it reached a `fail`.
    pub failure: Option<FailureSummary>,
    /// Step executions.
    pub steps: u64,
    /// Start counts by step id.
    pub step_counts: BTreeMap<String, u64>,
    /// How many effects the machine emitted (none were executed).
    pub effects: usize,
    /// The replayed trace.
    pub trace: Vec<sven_sdk::TraceRecord>,
}

/// The longest a single run may take before the testkit gives up on it. A
/// hanging fixture without a bound is a case bug; this turns it into an
/// error instead of a stuck test run.
const SAFETY_LIMIT: Duration = Duration::from_secs(120);

/// How long a finished run is given for abandoned fixture calls to be
/// dropped before they are reported as leaked.
const LEAK_GRACE: Duration = Duration::from_secs(2);

/// Runs the harness in `dir` once.
///
/// # Errors
///
/// [`TestkitError`] when the harness does not load, the inputs do not fit,
/// a fixture is unusable, a live model lacks its opt-in, or the run could
/// not be carried out. A plan's own failure is a result, not an error.
pub async fn run_once(dir: &HarnessDir, request: RunRequest) -> Result<RunResult, TestkitError> {
    let prepared = Prepared::new(dir, &request)?;
    let mut run = prepared
        .harness
        .start(request.input.clone())
        .map_err(|e| TestkitError::Harness(e.to_string()))?;
    if request.record {
        run = run.with_recording();
    }
    prepared.drive(run, &request).await
}

/// Resumes a checkpoint of the harness in `dir`; `request.input` is
/// ignored (the checkpoint carries the inputs).
///
/// # Errors
///
/// As [`run_once`], plus a checkpoint taken under another program.
pub async fn resume_once(
    dir: &HarnessDir,
    checkpoint: HarnessCheckpoint,
    request: RunRequest,
) -> Result<RunResult, TestkitError> {
    let prepared = Prepared::new(dir, &request)?;
    let mut run = prepared
        .harness
        .resume(checkpoint)
        .map_err(|e| TestkitError::Harness(e.to_string()))?;
    if request.record {
        run = run.with_recording();
    }
    prepared.drive(run, &request).await
}

/// Replays a recording under the harness in `dir`, offline: nothing asks a
/// model, runs a tool or starts a child.
///
/// # Errors
///
/// [`TestkitError::Harness`] when the harness does not load or the
/// recording was taken under another program.
pub fn replay_once(
    dir: &HarnessDir,
    recording: &HarnessRecording,
) -> Result<ReplaySummary, TestkitError> {
    let engine = Engine::builder()
        .config(Config::default())
        .model_provider(Arc::new(FailingMockProvider::new(
            "replay executes nothing",
        )))
        .toolset(Toolset::none())
        .build()
        .map_err(|e| TestkitError::Run(e.to_string()))?;
    let (harness, _) = load_all(&engine, dir)?;
    let replay = harness
        .replay(recording)
        .map_err(|e| TestkitError::Harness(e.to_string()))?;
    let (output, failure) = verdict(replay.result.as_ref());
    Ok(ReplaySummary {
        state: replay.state.clone(),
        output,
        failure,
        steps: replay.steps,
        step_counts: replay.step_counts(),
        effects: replay.effects.len(),
        trace: replay.trace,
    })
}

/// An engine with the request's fixtures, and the harness loaded on it.
struct Prepared {
    harness: Harness,
    bundle_digest: Option<String>,
    books: Arc<ToolBooks>,
    model: BuiltModel,
    /// Keeps a fresh workspace alive for the run.
    _workspace: Option<tempfile::TempDir>,
}

impl Prepared {
    fn new(dir: &HarnessDir, request: &RunRequest) -> Result<Self, TestkitError> {
        let model = fixture_model::build(request.model.as_ref(), dir, &request.opt_in)?;
        let (root, keep) = match &request.workspace {
            Workspace::Fresh(files) => {
                let tmp = tempfile::tempdir().map_err(|source| TestkitError::Io {
                    path: std::env::temp_dir(),
                    source,
                })?;
                seed_workspace(tmp.path(), files)?;
                (tmp.path().to_path_buf(), Some(tmp))
            }
            Workspace::Dir(path) => (path.clone(), None),
        };
        let books = Arc::new(ToolBooks::default());
        // Config::default() and an injected provider: the user's own
        // configuration is never read, so a run cannot reach a model it was
        // not given.
        let mut builder = Engine::builder()
            .config(Config::default())
            .model_provider(Arc::clone(&model.provider))
            .toolset(Toolset::none())
            .project_root(&root)
            .approvals(ApprovalPolicy::Auto);
        for (name, fixture) in &request.tools {
            let tool = FixtureTool::new(name, fixture, root.clone(), Arc::clone(&books));
            builder = builder.tool(Arc::new(tool));
        }
        let engine = builder
            .build()
            .map_err(|e| TestkitError::Run(e.to_string()))?;
        let (harness, main) = load_all(&engine, dir)?;
        Ok(Self {
            harness,
            bundle_digest: main.bundle_digest,
            books,
            model,
            _workspace: keep,
        })
    }

    async fn drive(
        &self,
        mut run: HarnessRun,
        request: &RunRequest,
    ) -> Result<RunResult, TestkitError> {
        let cancel = CancelToken::new();
        let mut options = RunOptions::new().cancel(cancel.clone());
        if let Some(ms) = request.bounds.deadline_ms {
            options = options.deadline(Duration::from_millis(ms));
        }
        if let Some(tokens) = request.bounds.max_output_tokens {
            options = options.max_output_tokens(tokens);
        }
        let timer = request.bounds.cancel_after_ms.map(|ms| {
            let token = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                token.cancel();
            })
        });

        let started = Instant::now();
        let executed = tokio::time::timeout(SAFETY_LIMIT, run.execute(options)).await;
        if let Some(timer) = timer {
            timer.abort();
        }
        let outcome = executed
            .map_err(|_| {
                TestkitError::Run(format!(
                    "the run exceeded the testkit's {}s safety limit",
                    SAFETY_LIMIT.as_secs()
                ))
            })?
            .map_err(|e| TestkitError::Run(e.to_string()))?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let leaked = self.settle_fixtures().await;
        let mut problems = self.books.problems();
        problems.extend(self.model.usage.exhausted());
        Ok(RunResult {
            summary: summarize(&self.harness, &outcome, self.books.invoked_names()),
            checkpoint: run.checkpoint(),
            recording: run.recording(),
            trajectory: if request.trajectory {
                run.trajectory().and_then(|t| serde_json::to_value(t).ok())
            } else {
                None
            },
            run_id: run.run_id().to_string(),
            bundle_digest: self.bundle_digest.clone(),
            duration_ms,
            fixtures: FixtureFacts {
                capabilities: self.books.invoked_capabilities(),
                leaked,
                problems,
            },
            outcome,
        })
    }

    /// Waits briefly for fixture calls abandoned by a stopped run to be
    /// dropped; what is still running afterwards is leaked.
    async fn settle_fixtures(&self) -> usize {
        let deadline = Instant::now() + LEAK_GRACE;
        while self.books.in_flight() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.books.in_flight()
    }
}

/// Writes `files` (relative path to text) under `root`.
pub(crate) fn seed_workspace(
    root: &Path,
    files: &BTreeMap<String, String>,
) -> Result<(), TestkitError> {
    for (rel, text) in files {
        let path = jail_join(root, rel)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| TestkitError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&path, text).map_err(|source| TestkitError::Io { path, source })?;
    }
    Ok(())
}

/// Loads the directory's children, then its main document, onto `engine`.
fn load_all(engine: &Engine, dir: &HarnessDir) -> Result<(Harness, Source), TestkitError> {
    let load = |source: &Source| {
        engine
            .load_harness_from_str(&source.text)
            .map_err(|e| TestkitError::Harness(format!("{}: {e}", source.path.display())))
    };
    for child in &dir.children {
        load(&Source::read(child)?)?;
    }
    let main = Source::read(&dir.main)?;
    Ok((load(&main)?, main))
}

fn verdict(
    result: Option<&Result<Value, HarnessFailure>>,
) -> (Option<Value>, Option<FailureSummary>) {
    match result {
        Some(Ok(value)) => (Some(value.clone()), None),
        Some(Err(f)) => (
            None,
            Some(FailureSummary {
                code: f.code.clone(),
                message: f.message.clone(),
            }),
        ),
        None => (None, None),
    }
}

fn summarize(
    harness: &Harness,
    outcome: &HarnessOutcome,
    tools_invoked: Vec<String>,
) -> RunSummary {
    let (output, mut failure) = verdict(outcome.result.as_ref());
    let status = match (&outcome.conclusion, &outcome.result) {
        (RunConclusion::Cancelled, _) => RunStatus::Cancelled,
        (RunConclusion::Timeout | RunConclusion::BudgetExhausted, None) => RunStatus::Suspended,
        (_, Some(Ok(_))) => RunStatus::Completed,
        (_, Some(Err(_))) => RunStatus::Failed,
        (other, None) => {
            failure = Some(FailureSummary {
                code: "no_verdict".to_string(),
                message: format!("the run ended ({other:?}) without a verdict"),
            });
            RunStatus::Failed
        }
    };
    RunSummary {
        status,
        output,
        failure,
        steps: outcome.steps,
        step_counts: outcome.step_counts(),
        tools_invoked,
        input_tokens: outcome.usage.input_tokens,
        output_tokens: outcome.usage.output_tokens,
        plan_digest: harness.digest().to_string(),
    }
}
