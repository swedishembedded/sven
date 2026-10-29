// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run` and `resume`: one run, its files, its report and its
//! exit code.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use sven_sdk::{sourced_trace, HarnessCheckpoint, SourcedRecord};

use super::engine::{self, Assembly};
use super::engine_run::{RunResult, RunStatus, RunSummary};
use super::live_files::conversation_file;
use super::source::Source;
use super::stamps::{stamped_line, Stamps};
use super::{kit, print_json, usage, Failure, Outcome};
use crate::cli::HarnessRunArgs;

/// What the run's observer saw: when each trace record arrived, and whether
/// the progress lines were printed as they did.
struct Seen<'a> {
    stamps: &'a Stamps,
    printed: bool,
}

/// Starts the harness at `path` and reports how it ended.
pub(super) async fn run(path: &Path, args: &HarnessRunArgs) -> Outcome {
    let plan = Arc::new(Source::open(path).map_err(kit)?.plan);
    let stamps = Stamps::begin();
    let assembly = Assembly::from_args(args, plan, &stamps, false)?;
    let printed = assembly.streams_progress();
    let result = engine::run(path, assembly).await?;
    finish(
        args,
        &result,
        Seen {
            stamps: &stamps,
            printed,
        },
    )
}

/// Resumes the checkpoint in `checkpoint` under the harness at `path`.
pub(super) async fn resume(path: &Path, checkpoint: &Path, args: &HarnessRunArgs) -> Outcome {
    let plan = Arc::new(Source::open(path).map_err(kit)?.plan);
    let text = std::fs::read_to_string(checkpoint)
        .map_err(|e| usage(format!("cannot read {}: {e}", checkpoint.display())))?;
    let checkpoint: HarnessCheckpoint = serde_json::from_str(&text)
        .map_err(|e| usage(format!("{} is not a checkpoint: {e}", checkpoint.display())))?;
    let stamps = Stamps::begin();
    let assembly = Assembly::from_args(args, plan, &stamps, true)?;
    let printed = assembly.streams_progress();
    let result = engine::resume(path, checkpoint, assembly).await?;
    finish(
        args,
        &result,
        Seen {
            stamps: &stamps,
            printed,
        },
    )
}

/// Writes the files the flags asked for, prints the report, and maps the
/// run's status to its exit code.
///
/// `seen` says when the trace records were observed, and whether the progress
/// lines were already printed while the run went (so they are not repeated
/// from the trace afterwards).
fn finish(args: &HarnessRunArgs, result: &RunResult, seen: Seen<'_>) -> Outcome {
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
    if args.json {
        print_json(&report(args, result))?;
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
    if let (Some(dir), Some(trajectory)) = (&args.conversations, &result.trajectory) {
        write_conversations(dir, trajectory)?;
    }
    Ok(())
}

/// Writes each conversation embedded in `trajectory` to `dir` as its own
/// ATIF file, numbered in the order the conversations began.
fn write_conversations(
    dir: &std::path::Path,
    trajectory: &serde_json::Value,
) -> Result<(), Failure> {
    std::fs::create_dir_all(dir).map_err(|e| Failure {
        code: 5,
        message: format!("cannot create {}: {e}", dir.display()),
    })?;
    let embedded = trajectory["subagent_trajectories"]
        .as_array()
        .into_iter()
        .flatten();
    for (index, conversation) in embedded
        .filter(|t| t["extra"]["harness"]["conversation"].is_string())
        .enumerate()
    {
        let name = conversation["extra"]["harness"]["conversation"]
            .as_str()
            .unwrap_or_default();
        write(
            &dir.join(conversation_file(index, name)),
            &to_json(conversation)?,
        )?;
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
fn report(args: &HarnessRunArgs, result: &RunResult) -> Value {
    let summary = &result.summary;
    let written = args
        .checkpoint_out
        .as_ref()
        .filter(|_| result.checkpoint.is_some());
    json!({
        "harness": result.plan.name,
        "plan_digest": summary.plan_digest,
        "bundle_digest": result.bundle_digest,
        "run_id": result.run_id,
        "status": summary.status,
        "conclusion": snake(&format!("{:?}", result.outcome.conclusion)),
        "output": summary.output,
        "failure": summary.failure,
        "usage": {
            "input_tokens": summary.input_tokens,
            "cached_tokens": summary.cached_tokens,
            "output_tokens": summary.output_tokens,
            "cost_usd": summary.cost_usd,
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
