// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One run of a harness on an engine the caller built, and offline replay.
//!
//! The caller assembles the [`Engine`] (model, tools, workspace, approvals,
//! host ceiling); this module loads the harness onto it, drives the run to a
//! verdict or a bound and summarizes it. Nothing bounds a run except what the
//! caller sets in [`EngineRun`] and what the harness declares.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_model::{CompletionRequest, ModelProvider, ResponseStream};
use sven_sdk::config::Config;
use sven_sdk::{
    Engine, Harness, HarnessCheckpoint, HarnessFailure, HarnessOutcome, HarnessRecording,
    HarnessRun, RunConclusion, RunOptions, Toolset,
};

use super::error::HarnessError;
use super::source::Source;

/// How a harness run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RunStatus {
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
pub(super) struct FailureSummary {
    /// The failure code.
    pub code: String,
    /// The failure message.
    pub message: String,
}

/// What one run did, in JSON-ready form.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct RunSummary {
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
    /// Prompt tokens the provider reported, excluding prompt-cache hits.
    pub input_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache.
    pub cached_tokens: Option<u64>,
    /// What the provider charged, in USD, when it says.
    pub cost_usd: Option<f64>,
    /// Generated tokens the provider reported, when it reported any.
    pub output_tokens: Option<u64>,
    /// The plan's digest.
    pub plan_digest: String,
}

/// What an offline replay reproduced.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) struct ReplaySummary {
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

/// What a single run produced.
#[derive(Debug)]
pub(super) struct RunResult {
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
    /// The run's private directory.
    pub scratchpad: Option<PathBuf>,
    /// The bundle digest, when the harness is a bundle or a package.
    pub bundle_digest: Option<String>,
    /// Wall-clock time.
    pub duration_ms: u64,
    /// The corrections the run's supervisors sent, in order.
    pub corrections: Vec<sven_sdk::Correction>,
    /// The supervisor messages an accept gate refused.
    pub refused: Vec<sven_sdk::RefusedMessage>,
    /// The plan that ran, which reads its trace in the author's terms.
    pub plan: sven_sdk::harness_plan::HarnessPlan,
}

/// How a run on a caller-built engine starts and is bounded.
#[derive(Clone, Default)]
pub(super) struct EngineRun {
    /// The run's inputs (checked against the harness's declared inputs);
    /// ignored by a resume, whose checkpoint carries them.
    pub input: Value,
    /// Suspend the run (leaving a checkpoint) after this long.
    pub deadline_ms: Option<u64>,
    /// Suspend the run once the model has generated this many tokens.
    pub max_output_tokens: Option<u64>,
    /// Keep a recording of the machine's input events.
    pub record: bool,
    /// Build an ATIF trajectory after the run.
    pub trajectory: bool,
    /// The run's private directory (`{{ run.scratchpad }}`); without one the
    /// SDK's default is used and kept.
    pub scratchpad: Option<PathBuf>,
    /// Called with each trace record as the run produces it, so a long run
    /// can report progress while it is still going.
    pub progress: Option<ProgressFn>,
    /// Called with each session event the run publishes while it goes: the
    /// agent's tool calls and their results, token usage. Unlike the trace,
    /// which the run publishes when its machine is at rest, these arrive
    /// during a long model turn or tool call.
    pub activity: Option<ActivityFn>,
    /// Suspends the run, leaving a checkpoint, once this token is
    /// cancelled (a host's signal handler, say).
    pub suspend: Option<sven_sdk::CancelToken>,
    /// Called with a checkpoint each time a `for_each` item completes, for a
    /// host that keeps the latest one durable so a killed run resumes at the
    /// item it was on.
    pub item_checkpoint: Option<CheckpointFn>,
    /// Called with the conversations that changed each time a `for_each`
    /// item finishes, for a host that keeps them durable while the run goes.
    pub item_conversations: Option<ConversationsFn>,
}

/// A trace observer; see [`EngineRun::progress`].
pub(super) type ProgressFn = Arc<dyn Fn(&sven_sdk::TraceRecord) + Send + Sync>;

/// A checkpoint observer; see [`EngineRun::item_checkpoint`].
pub(super) type CheckpointFn = Arc<dyn Fn(&HarnessCheckpoint) + Send + Sync>;

/// A conversation observer; see [`EngineRun::item_conversations`].
pub(super) type ConversationsFn = Arc<dyn Fn(&[sven_sdk::ConversationUpdate]) + Send + Sync>;

/// A session-event observer; see [`EngineRun::activity`].
pub(super) type ActivityFn = Arc<dyn Fn(&sven_sdk::SessionEvent) + Send + Sync>;

/// Runs the harness at `path` once on `engine`.
///
/// # Errors
///
/// [`HarnessError`] when the harness does not load, the inputs do not fit it
/// or the run could not be carried out. A plan's own failure is a result, not
/// an error.
pub(super) async fn run_on_engine(
    engine: &Engine,
    path: &Path,
    request: EngineRun,
) -> Result<RunResult, HarnessError> {
    let (harness, main) = load(engine, path)?;
    let run = harness
        .start(request.input.clone())
        .map_err(|e| HarnessError::Harness(e.to_string()))?;
    drive(&harness, main.bundle_digest, run, request).await
}

/// Resumes `checkpoint` of the harness at `path` on `engine`.
///
/// # Errors
///
/// As [`run_on_engine`], plus a checkpoint taken under another program.
pub(super) async fn resume_on_engine(
    engine: &Engine,
    path: &Path,
    checkpoint: HarnessCheckpoint,
    request: EngineRun,
) -> Result<RunResult, HarnessError> {
    let (harness, main) = load(engine, path)?;
    let run = harness
        .resume(checkpoint)
        .map_err(|e| HarnessError::Harness(e.to_string()))?;
    drive(&harness, main.bundle_digest, run, request).await
}

/// A model that is never asked: replay executes nothing, but an engine needs
/// one to load a harness.
struct NoModel;

#[async_trait]
impl ModelProvider for NoModel {
    fn name(&self) -> &str {
        "replay"
    }
    fn model_name(&self) -> &str {
        "none"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        Err(anyhow::anyhow!("replay executes nothing"))
    }
}

/// Replays a recording under the harness at `path`, offline: nothing asks a
/// model, runs a tool or starts a child.
///
/// # Errors
///
/// [`HarnessError::Harness`] when the harness does not load or the
/// recording was taken under another program.
pub(super) fn replay_once(
    path: &Path,
    recording: &HarnessRecording,
) -> Result<ReplaySummary, HarnessError> {
    let engine = Engine::builder()
        .config(Config::default())
        .model_provider(Arc::new(NoModel))
        .toolset(Toolset::none())
        .build()
        .map_err(|e| HarnessError::Run(e.to_string()))?;
    let (harness, _) = load(&engine, path)?;
    let replay = harness
        .replay(recording)
        .map_err(|e| HarnessError::Harness(e.to_string()))?;
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

/// Loads the main document at `path` (with what it imports) onto `engine`.
fn load(engine: &Engine, path: &Path) -> Result<(Harness, Source), HarnessError> {
    let main = Source::open(path)?;
    let harness = engine
        .load_harness_from_str(&main.text)
        .map_err(|e| HarnessError::Harness(format!("{}: {e}", main.path.display())))?;
    Ok((harness, main))
}

/// The next item of an optional broadcast subscription: pends forever when
/// there is none, skips what a slow reader missed, and drops the
/// subscription once its channel closes.
async fn heard<T: Clone>(feed: &mut Option<tokio::sync::broadcast::Receiver<T>>) -> T {
    use tokio::sync::broadcast::error::RecvError;
    loop {
        let Some(receiver) = feed.as_mut() else {
            return std::future::pending().await;
        };
        match receiver.recv().await {
            Ok(item) => return item,
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => *feed = None,
        }
    }
}

/// Executes `run`, telling `progress` of its trace records and `activity` of
/// its session events as they appear.
async fn execute_observed(
    run: &mut HarnessRun,
    options: RunOptions,
    progress: Option<&ProgressFn>,
    activity: Option<&ActivityFn>,
) -> Result<HarnessOutcome, sven_sdk::CallError> {
    if progress.is_none() && activity.is_none() {
        return run.execute(options).await;
    }
    let mut records = progress.map(|_| run.subscribe_trace());
    let mut events = activity.map(|_| run.events());
    let mut execution = Box::pin(run.execute(options));
    let outcome = loop {
        tokio::select! {
            outcome = &mut execution => break outcome,
            record = heard(&mut records) => {
                if let Some(progress) = progress {
                    progress(&record);
                }
            }
            event = heard(&mut events) => {
                if let Some(activity) = activity {
                    activity(&event);
                }
            }
        }
    };
    if let (Some(progress), Some(records)) = (progress, records.as_mut()) {
        while let Ok(record) = records.try_recv() {
            progress(&record);
        }
    }
    outcome
}

/// Drives `run` to its end under the bounds of `request`.
async fn drive(
    harness: &Harness,
    bundle_digest: Option<String>,
    mut run: HarnessRun,
    request: EngineRun,
) -> Result<RunResult, HarnessError> {
    if request.record {
        run = run.with_recording();
    }
    if let Some(dir) = request.scratchpad {
        run = run.with_scratchpad(dir);
    }
    if let Some(sink) = request.item_checkpoint {
        run = run.on_item_checkpoint(move |checkpoint| sink(&checkpoint));
    }
    if let Some(sink) = request.item_conversations {
        run = run.on_item_conversations(move |updates| sink(&updates));
    }
    let mut options = RunOptions::new();
    if let Some(ms) = request.deadline_ms {
        options = options.deadline(Duration::from_millis(ms));
    }
    if let Some(tokens) = request.max_output_tokens {
        options = options.max_output_tokens(tokens);
    }
    if let Some(token) = request.suspend {
        options = options.suspend_on(token);
    }
    let started = Instant::now();
    let outcome = execute_observed(
        &mut run,
        options,
        request.progress.as_ref(),
        request.activity.as_ref(),
    )
    .await
    .map_err(|e| HarnessError::Run(e.to_string()))?;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(RunResult {
        summary: summarize(harness, &outcome),
        checkpoint: run.checkpoint(),
        recording: run.recording(),
        trajectory: if request.trajectory {
            run.trajectory().and_then(|t| serde_json::to_value(t).ok())
        } else {
            None
        },
        run_id: run.run_id().to_string(),
        scratchpad: run.scratchpad().map(Path::to_path_buf),
        bundle_digest,
        duration_ms,
        corrections: run.corrections().unwrap_or_default(),
        refused: run.refused_messages().unwrap_or_default(),
        plan: harness.plan().clone(),
        outcome,
    })
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

fn summarize(harness: &Harness, outcome: &HarnessOutcome) -> RunSummary {
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
        input_tokens: outcome.usage.input_tokens,
        cached_tokens: outcome.usage.cached_tokens,
        cost_usd: outcome.usage.cost_usd,
        output_tokens: outcome.usage.output_tokens,
        plan_digest: harness.digest().to_string(),
    }
}
