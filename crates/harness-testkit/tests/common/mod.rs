// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared fixtures for the testkit's specs: harness documents and a helper
//! that writes a harness directory into a tempdir.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.
#![allow(dead_code)]

use std::path::Path;

use sven_harness_testkit::HarnessDir;
use tempfile::TempDir;

/// Writes `files` (relative path, text) under `root`, creating directories.
pub fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (rel, text) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, text).expect("write fixture");
    }
}

/// A tempdir holding `files`, opened as a harness directory.
pub fn harness_dir(files: &[(&str, &str)]) -> (TempDir, HarnessDir) {
    let tmp = tempfile::tempdir().expect("a tempdir");
    write_tree(tmp.path(), files);
    let dir = HarnessDir::open(tmp.path()).expect("a valid harness directory");
    (tmp, dir)
}

/// One schema-constrained model turn that labels a ticket; the run returns the
/// label and the queue it names.
pub const ROUTING: &str = r#"
api_version: "2"
name: model-routing
description: "Classify a ticket with one schema-constrained model turn."
inputs: { ticket: string }
outputs:
  queue: { type: string, value: "{{ tasks.classify.output.label }}-queue" }
  label: { type: string, value: "{{ tasks.classify.output.label }}" }
execution: { limits: { model_turns: 2 } }
workflow:
  - name: classify
    model_only: true
    schema: { type: object, required: [label], properties: { label: { type: string, enum: [bug, feature, question] } } }
    task: "Classify as bug, feature or question: {{ inputs.ticket }}"
"#;

/// The routing harness with the queue suffix renamed: a variant that changes
/// every output.
pub fn routing_with_queue_suffix(suffix: &str) -> String {
    ROUTING.replace("-queue", &format!("-{suffix}"))
}

/// The routing harness followed by a review turn that has the last word on
/// the label: a candidate that spends a second model turn on every ticket.
pub fn routing_with_review() -> String {
    ROUTING
        .replace("model_turns: 2", "model_turns: 4")
        .replace(
            "outputs:\n  queue: { type: string, value: \"{{ tasks.classify.output.label }}-queue\" }\n  label: { type: string, value: \"{{ tasks.classify.output.label }}\" }\n",
            "outputs:\n  queue: { type: string, value: \"{{ tasks.review.output }}-queue\" }\n  label: { type: string, value: \"{{ tasks.review.output }}\" }\n",
        )
        + "  - name: review\n    model_only: true\n    task: \"Answer with the label only: {{ tasks.classify.output.label }}\"\n"
}

pub const ROUTING_BUG_CASE: &str = r#"
input: { ticket: "The app crashes on start" }
model:
  sequence:
    - text: '{"label": "bug"}'
      usage: { input: 20, output: 6 }
expect:
  status: completed
  output: { queue: bug-queue, label: bug }
  steps: { classify__work: 1 }
  max_output_tokens: 100
"#;

pub const ROUTING_FEATURE_CASE: &str = r#"
input: { ticket: "Please add dark mode" }
model:
  sequence:
    - text: '{"label": "feature"}'
expect:
  status: completed
  output: { queue: feature-queue, label: feature }
  steps: { classify__work: 1 }
"#;

pub const ROUTING_UNROUTABLE_CASE: &str = r#"
input: { ticket: "???" }
model:
  sequence:
    - text: 'nope'
expect:
  status: failed
  failure: { code: invalid_answer, message_contains: "not valid JSON" }
"#;

/// An agent that patches a file, and a check that the file is there.
pub const REPAIR: &str = r#"
api_version: "2"
name: repair
agent: { profile: coding, tools: [write_patch] }
execution: { limits: { model_turns: 4, tool_calls: 4 } }
workflow:
  - name: patch
    task: Patch it.
    verify: [{ artifact: fixed.txt }]
outputs:
  note: { type: string, value: "{{ tasks.patch.output }}" }
"#;

/// The turns of an agent that calls `write_patch` once and says it is done.
pub const PATCHING_MODEL: &str = r#"
model:
  sequence:
    - tool_calls: [ { id: p1, name: write_patch, args: {} } ]
    - text: done
"#;

/// A check task: a gate runs and its output is returned. The gate is a
/// fixture tool the case scripts, which can be parked on to suspend and
/// resume the run.
pub const REPORT: &str = r#"
api_version: "2"
name: report
execution: { limits: { model_turns: 2 } }
gates:
  read: { command: [cat, report.txt] }
workflow:
  - name: fetch
    verify: [read]
outputs:
  report: { type: string, value: "{{ gates.read.stdout }}" }
"#;

/// Two model turns in a row.
pub const TWO_TURNS: &str = r#"
api_version: "2"
name: two-turns
inputs: { ticket: string }
execution: { limits: { model_turns: 3 } }
workflow:
  - name: first
    model_only: true
    task: first
  - name: second
    model_only: true
    task: second
"#;
