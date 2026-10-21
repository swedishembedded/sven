// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run` and `resume`: one run, its files, its report and its
//! exit code.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use sven_harness_testkit::{
    inspect, main_plan, resume_once, run_once, HarnessDir, RunResult, RunStatus, RunSummary,
};
use sven_sdk::{sourced_trace, HarnessCheckpoint, SourcedRecord};

use super::production::{self, Production};
use super::stamps::{stamped_line, Stamps};
use super::{kit, print_json, request, usage, Failure, Outcome};
use crate::cli::{HarnessRunArgs, ToolsetArg};

/// What the run's observer saw: when each trace record arrived, and whether
/// the progress lines were printed as they did.
struct Seen<'a> {
    stamps: &'a Stamps,
    printed: bool,
}

/// Starts the harness at `path` and reports how it ended.
pub(super) async fn run(path: &Path, args: &HarnessRunArgs) -> Outcome {
    let dir = HarnessDir::open(path).map_err(kit)?;
    if args.toolset == ToolsetArg::Coding {
        let plan = Arc::new(main_plan(&dir).map_err(kit)?);
        let stamps = Stamps::begin();
        let production = Production::from_args(args, plan, &stamps)?;
        let printed = production.streams_progress();
        let result = production::run(&dir, production).await?;
        return finish(
            path,
            args,
            &result,
            false,
            Seen {
                stamps: &stamps,
                printed,
            },
        );
    }
    let stamps = Stamps::begin();
    let request = request::build(args, stamps.observer())?;
    let no_model = request::is_default_model(&request);
    let result = run_once(&dir, request).await.map_err(kit)?;
    finish(
        path,
        args,
        &result,
        no_model,
        Seen {
            stamps: &stamps,
            printed: false,
        },
    )
}

/// Resumes the checkpoint in `checkpoint` under the harness at `path`.
pub(super) async fn resume(path: &Path, checkpoint: &Path, args: &HarnessRunArgs) -> Outcome {
    let dir = HarnessDir::open(path).map_err(kit)?;
    let text = std::fs::read_to_string(checkpoint)
        .map_err(|e| usage(format!("cannot read {}: {e}", checkpoint.display())))?;
    let checkpoint: HarnessCheckpoint = serde_json::from_str(&text)
        .map_err(|e| usage(format!("{} is not a checkpoint: {e}", checkpoint.display())))?;
    if args.toolset == ToolsetArg::Coding {
        let plan = Arc::new(main_plan(&dir).map_err(kit)?);
        let stamps = Stamps::begin();
        let production = Production::from_args(args, plan, &stamps)?;
        let printed = production.streams_progress();
        let result = production::resume(&dir, checkpoint, production).await?;
        return finish(
            path,
            args,
            &result,
            false,
            Seen {
                stamps: &stamps,
                printed,
            },
        );
    }
    let stamps = Stamps::begin();
    let request = request::build(args, stamps.observer())?;
    let no_model = request::is_default_model(&request);
    let result = resume_once(&dir, checkpoint, request).await.map_err(kit)?;
    finish(
        path,
        args,
        &result,
        no_model,
        Seen {
            stamps: &stamps,
            printed: false,
        },
    )
}

/// Writes the files the flags asked for, prints the report, and maps the
/// run's status to its exit code.
///
/// `seen` says when the trace records were observed, and whether the progress
/// lines were already printed while the run went (so they are not repeated
/// from the trace afterwards).
fn finish(
    path: &Path,
    args: &HarnessRunArgs,
    result: &RunResult,
    no_model: bool,
    seen: Seen<'_>,
) -> Outcome {
    let times = seen.stamps.times(result.outcome.trace.len());
    write_files(args, result, &times)?;
    if !args.quiet && !seen.printed {
        for (record, ts_ms) in sourced_trace(&result.plan, &result.outcome.trace)
            .into_iter()
            .zip(&times)
        {
            if let Ok(line) = stamped_line(&record, *ts_ms) {
                eprintln!("[sven:harness] {line}");
            }
        }
    }
    let summary = &result.summary;
    if summary.status == RunStatus::Suspended && args.checkpoint_out.is_none() {
        eprintln!("note: the run is suspended; pass --checkpoint-out FILE to keep its checkpoint");
    }
    if no_model && summary.status == RunStatus::Failed {
        // The model's own error is the plan's business (a retry may turn it
        // into the plan's failure); say what the default model was.
        eprintln!(
            "note: no model source was given (--model-fixture, --case model or --model), so every model call failed"
        );
    }
    if args.json {
        print_json(&report(path, args, result))?;
    } else {
        if let Some(dir) = &result.scratchpad {
            eprintln!("scratchpad: {}", dir.display());
        }
        print_human(summary);
    }
    Ok(match summary.status {
        RunStatus::Completed => 0,
        RunStatus::Failed => 1,
        RunStatus::Suspended => 3,
        RunStatus::Cancelled => 4,
    })
}

fn write_files(args: &HarnessRunArgs, result: &RunResult, times: &[u64]) -> Result<(), Failure> {
    if let Some(file) = &args.trace {
        let mut lines = String::new();
        for (record, ts_ms) in sourced_trace(&result.plan, &result.outcome.trace)
            .into_iter()
            .zip(times)
        {
            lines.push_str(&jsonl_line(&record, *ts_ms)?);
        }
        write(file, &lines)?;
    }
    if let Some(file) = &args.record {
        let recording = result.recording.as_ref().ok_or_else(|| Failure {
            code: 5,
            message: "the run produced no recording".to_string(),
        })?;
        write(file, &to_json(recording)?)?;
    }
    if let (Some(file), Some(checkpoint)) = (&args.checkpoint_out, &result.checkpoint) {
        write(file, &to_json(checkpoint)?)?;
    }
    if let (Some(file), Some(trajectory)) = (&args.trajectory, &result.trajectory) {
        write(file, &to_json(trajectory)?)?;
    }
    Ok(())
}

fn jsonl_line(record: &SourcedRecord, ts_ms: u64) -> Result<String, Failure> {
    stamped_line(record, ts_ms)
        .map(|line| line + "\n")
        .map_err(|e| Failure {
            code: 5,
            message: format!("cannot serialize a trace record: {e}"),
        })
}

fn to_json(value: &impl serde::Serialize) -> Result<String, Failure> {
    serde_json::to_string_pretty(value).map_err(|e| Failure {
        code: 5,
        message: format!("cannot serialize: {e}"),
    })
}

fn write(path: &Path, text: &str) -> Result<(), Failure> {
    std::fs::write(path, text).map_err(|e| usage(format!("cannot write {}: {e}", path.display())))
}

/// The `--json` document. `checkpoint` is the file the checkpoint was
/// written to (`--checkpoint-out`), null when none was written.
fn report(path: &Path, args: &HarnessRunArgs, result: &RunResult) -> Value {
    let summary = &result.summary;
    let name = inspect(path).map(|i| i.name).unwrap_or_default();
    let written = args
        .checkpoint_out
        .as_ref()
        .filter(|_| result.checkpoint.is_some());
    json!({
        "harness": name,
        "plan_digest": summary.plan_digest,
        "bundle_digest": result.bundle_digest,
        "run_id": result.run_id,
        "status": summary.status,
        "conclusion": snake(&format!("{:?}", result.outcome.conclusion)),
        "output": summary.output,
        "failure": summary.failure,
        "usage": {
            "input_tokens": summary.input_tokens,
            "output_tokens": summary.output_tokens,
        },
        "steps": summary.steps,
        "step_counts": summary.step_counts,
        "duration_ms": result.duration_ms,
        "corrections": result.corrections,
        "refused_messages": result.refused,
        "checkpoint": written,
        "scratchpad": result.scratchpad,
    })
}

fn print_human(summary: &RunSummary) {
    println!("status: {}", snake(&format!("{:?}", summary.status)));
    if let Some(output) = &summary.output {
        println!("output: {output}");
    }
    if let Some(failure) = &summary.failure {
        println!("failure: {} - {}", failure.code, failure.message);
    }
    println!("steps: {}", summary.steps);
}

/// `BudgetExhausted` as `budget_exhausted`.
fn snake(word: &str) -> String {
    let mut out = String::new();
    for (i, c) in word.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
