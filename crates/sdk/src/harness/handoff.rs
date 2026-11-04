// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a harness run hands its host and requires of it: each finished
//! task's published files with a checkpoint taken right after, and, before a
//! fresh run starts, the files its `file` inputs name.
//!
//! Swedish Embedded AB implements solutions for handing finished work from
//! an automated run to the system around it for its clients. If your team
//! needs expertise in resumable agent workflows then you can procure our
//! services by sending an email to info@swedishembedded.com.

use serde_json::Value;
use sven_harness_runtime::TraceRecord;
use sven_hsm::Snapshot;

use super::HarnessCheckpoint;
use crate::error::CallError;

/// A finished task's files, handed to the host with a checkpoint taken right
/// after: keep both together and a run killed later resumes past the task,
/// with the files it left.
#[derive(Clone, Debug)]
pub struct Publication {
    /// The publish step.
    pub step: String,
    /// The task whose files these are.
    pub task: String,
    /// Each file's workspace-relative path and digest.
    pub files: Vec<sven_vocab::PublishedFile>,
    /// The run as it rests after the publication.
    pub checkpoint: HarnessCheckpoint,
}

pub(super) type PublicationSink = Box<dyn Fn(Publication) + Send + Sync>;

/// Gives `sink` each publication `records` hold, with the run as it rests at
/// `snapshot`.
pub(super) fn hand_over(
    records: &[TraceRecord],
    snapshot: &Snapshot,
    plan_digest: &str,
    run_id: &str,
    sink: &PublicationSink,
) {
    for record in records {
        if let TraceRecord::Published { step, task, files } = record {
            sink(Publication {
                step: step.clone(),
                task: task.clone(),
                files: files.clone(),
                checkpoint: super::run_support::checkpoint_at(
                    snapshot.clone(),
                    plan_digest,
                    run_id,
                ),
            });
        }
    }
}

/// A fresh run's `file` inputs must name files that are in its workspace
/// before anything starts: the host lays the tree out, the harness reads it.
pub(super) fn check_input_files(
    plan: &sven_harness_spec::HarnessPlan,
    inputs: &Value,
    root: Option<&std::path::Path>,
) -> Result<(), CallError> {
    let Some(declared) = &plan.inputs else {
        return Ok(());
    };
    let mut problems = Vec::new();
    for (name, input) in declared {
        if input.ty != sven_harness_spec::IOType::File {
            continue;
        }
        let Some(path) = inputs.get(name).and_then(Value::as_str) else {
            continue;
        };
        let Some(root) = root else {
            problems.push(format!(
                "`{name}` names a file, but the run has no workspace"
            ));
            continue;
        };
        let inside = root
            .join(path)
            .canonicalize()
            .ok()
            .filter(|real| root.canonicalize().is_ok_and(|r| real.starts_with(r)));
        match inside {
            Some(real) if real.is_file() => {}
            Some(_) => problems.push(format!("`{name}`: {path} is not a file")),
            None => problems.push(format!("`{name}`: {path} is not a file in the workspace")),
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(CallError::Precondition(problems.join("; ")))
    }
}
