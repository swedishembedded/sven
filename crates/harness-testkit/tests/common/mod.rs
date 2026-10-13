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

/// A schema-constrained model turn, retried once, then routed.
pub const ROUTING: &str = r#"
api_version: "1"
name: model-routing
description: "Classify a ticket with one schema-constrained model turn, then route it."
inputs: { ticket: string }
outputs: { queue: string, label: string }
schemas:
  route: { type: object, required: [label], properties: { label: { type: string, enum: [bug, feature, question] } } }
limits: { max_steps: 20, max_model_turns: 2 }
entry: classify
steps:
  classify: { retry: { attempts: 2, body: ask, then: route, on_exhausted: unroutable } }
  ask: { predict: { prompt: "Classify as bug, feature or question: {{ inputs.ticket }}", schema: route, store_as: route, then: route } }
  route: { branch: { on: { eq: "facts.route.label", value: "bug" }, when_true: to_bugs, when_false: to_backlog } }
  to_bugs: { return: { value: { queue: bugs, label: "{{ facts.route.label }}" } } }
  to_backlog: { return: { value: { queue: backlog, label: "{{ facts.route.label }}" } } }
  unroutable: { fail: { code: unroutable, message: "the model never produced a valid route" } }
"#;

/// The routing harness with the bug queue renamed: a variant that changes
/// one output.
pub fn routing_with_bug_queue(queue: &str) -> String {
    ROUTING.replace("queue: bugs", &format!("queue: {queue}"))
}

pub const ROUTING_BUG_CASE: &str = r#"
input: { ticket: "The app crashes on start" }
model:
  sequence:
    - text: '{"label": "bug"}'
      usage: { input: 20, output: 6 }
expect:
  status: completed
  output: { queue: bugs, label: bug }
  steps: { classify: 1, ask: 1 }
  max_output_tokens: 100
"#;

pub const ROUTING_FEATURE_CASE: &str = r#"
input: { ticket: "Please add dark mode" }
model:
  sequence:
    - text: 'not json at all'
    - text: '{"label": "feature"}'
expect:
  status: completed
  output: { queue: backlog, label: feature }
  steps: { ask: 2 }
"#;

pub const ROUTING_UNROUTABLE_CASE: &str = r#"
input: { ticket: "???" }
model:
  sequence:
    - text: 'nope'
    - text: 'still nope'
expect:
  status: failed
  failure: { code: unroutable, message_contains: "valid route" }
"#;

/// A tool step followed by a verification of what the tool wrote.
pub const REPAIR: &str = r#"
api_version: "1"
name: repair
capabilities: [WriteFile, RunVerifier]
limits: { max_steps: 20 }
entry: patch
steps:
  patch: { tool: { name: write_patch, store_as: patched, then: check } }
  check: { verify: { spec: { kind: file_exists, path: "fixed.txt" }, store_as: verdict, then: done } }
  done: { return: { value: "{{ steps.check.value }}" } }
"#;

/// A retried read that can be parked on, to be suspended and resumed.
pub const REPORT: &str = r#"
api_version: "1"
name: report
capabilities: [ReadFile]
limits: { max_steps: 20 }
entry: attempt
steps:
  attempt: { retry: { attempts: 2, body: call, then: done, on_exhausted: give_up } }
  call: { tool: { name: read_report, store_as: report, then: done } }
  done: { return: { value: "{{ facts.report }}" } }
  give_up: { fail: { code: stuck, message: "the report never came" } }
"#;

/// Two predict branches joined in declared order.
pub const FANOUT: &str = r#"
api_version: "1"
name: fanout
capabilities: [SpawnChild]
limits: { max_steps: 50, max_parallel: 2, max_model_turns: 4 }
entry: split
steps:
  split: { parallel: { branches: [a, b], join: merge } }
  a: { predict: { prompt: "branch a", store_as: out, then: ret_a } }
  ret_a: { return: { value: "{{ facts.out }}" } }
  b: { predict: { prompt: "branch b", store_as: out, then: ret_b } }
  ret_b: { return: { value: "{{ facts.out }}" } }
  merge: { return: { value: "{{ steps.split.value }}" } }
"#;

/// Two model turns in a row.
pub const TWO_TURNS: &str = r#"
api_version: "1"
name: two-turns
limits: { max_steps: 20, max_model_turns: 3 }
inputs: { ticket: string }
entry: first
steps:
  first: { predict: { prompt: "first", store_as: one, then: second } }
  second: { predict: { prompt: "second", store_as: two, then: done } }
  done: { return: { value: "{{ facts.two }}" } }
"#;
