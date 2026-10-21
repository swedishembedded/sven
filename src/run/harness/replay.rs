// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness replay`: re-derive a recorded run's verdict and trace
//! offline.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::Path;

use sven_sdk::{HarnessRecording, TraceRecord};

use super::engine_run::replay_once;
use super::{kit, print_json, usage, Outcome};

/// Replays `recording` under the harness at `path`. With `expect_trace`
/// the replayed trace must equal that JSONL trace (exit 1 when it does not).
pub(super) fn replay(
    path: &Path,
    recording: &Path,
    expect_trace: Option<&Path>,
    json: bool,
) -> Outcome {
    let text = std::fs::read_to_string(recording)
        .map_err(|e| usage(format!("cannot read {}: {e}", recording.display())))?;
    let recording: HarnessRecording = serde_json::from_str(&text)
        .map_err(|e| usage(format!("{} is not a recording: {e}", recording.display())))?;
    let summary = replay_once(path, &recording).map_err(kit)?;

    let matches = expect_trace
        .map(|file| expected_trace(file).map(|expected| expected == summary.trace))
        .transpose()?;
    if json {
        let mut doc = serde_json::to_value(&summary).map_err(|e| usage(e.to_string()))?;
        doc["trace_matches"] = serde_json::json!(matches);
        print_json(&doc)?;
    } else {
        println!("state: {}", summary.state);
        if let Some(output) = &summary.output {
            println!("output: {output}");
        }
        if let Some(failure) = &summary.failure {
            println!("failure: {} - {}", failure.code, failure.message);
        }
        println!("steps: {}", summary.steps);
        if let Some(matched) = matches {
            println!("trace: {}", if matched { "matches" } else { "differs" });
        }
    }
    if matches == Some(false) {
        eprintln!("error: the replayed trace differs from the expected trace");
        return Ok(1);
    }
    Ok(0)
}

fn expected_trace(file: &Path) -> Result<Vec<TraceRecord>, super::Failure> {
    let text = std::fs::read_to_string(file)
        .map_err(|e| usage(format!("cannot read {}: {e}", file.display())))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|e| {
                usage(format!(
                    "{}:{}: not a trace record: {e}",
                    file.display(),
                    index + 1
                ))
            })
        })
        .collect()
}
