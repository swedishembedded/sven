// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for unattended agent runs that
// always leave a record, for its clients. If your team needs expertise in
// auditable agent pipelines then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a headless run given `--output-trace` leaves a trajectory there even
//! when it fails before a session exists (no API key, a malformed `network`
//! section): its outcome is `startup_error` and it names the error.

use std::process::{Command, Stdio};

fn failed_start(extra: &[&str], config: &str) -> (Option<i32>, serde_json::Value) {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace.json");
    std::fs::write(dir.path().join("c.yaml"), config).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sven"))
        .args(["--headless", "-c"])
        .arg(dir.path().join("c.yaml"))
        .args(["--state-root"])
        .arg(dir.path().join("root"))
        .args(["--output-trace"])
        .arg(&trace)
        .args(extra)
        .arg("do the task")
        .current_dir(dir.path())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("sven runs");
    let written = std::fs::read_to_string(&trace)
        .unwrap_or_else(|e| panic!("no record after {:?}: {e}", out.status));
    (
        out.status.code(),
        serde_json::from_str(&written).expect("a valid document"),
    )
}

#[test]
fn a_missing_api_key_leaves_a_startup_record() {
    let (code, doc) = failed_start(&[], "model:\n  provider: openai\n  name: x\n");
    assert_eq!(code, Some(1));
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
    assert!(doc["final_metrics"]["extra"]["error"]
        .as_str()
        .unwrap()
        .contains("API key"));
    assert_eq!(doc["steps"][0]["message"], "do the task");
}

#[test]
fn a_malformed_network_section_leaves_a_startup_record() {
    let (code, doc) = failed_start(
        &["--model", "mock"],
        "network:\n  allow_hosts: [\"https://bad\"]\n",
    );
    assert_eq!(code, Some(1));
    assert!(doc["final_metrics"]["extra"]["error"]
        .as_str()
        .unwrap()
        .contains("network.allow_hosts"));
}

/// Runs `sven` with `args` and `stdin` and returns the exit code, stderr and
/// the record at `--output-trace`.
fn refused(args: &[&str], stdin: &str) -> (Option<i32>, String, serde_json::Value) {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace.json");
    std::fs::write(dir.path().join("corrupt.json"), "{not json").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sven"))
        .args(["--headless", "--model", "mock", "--state-root"])
        .arg(dir.path().join("root"))
        .arg("--output-trace")
        .arg(&trace)
        .args(args)
        .current_dir(dir.path())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let written = std::fs::read_to_string(&trace)
        .unwrap_or_else(|e| panic!("no record after {:?}: {e}", out.status));
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        serde_json::from_str(&written).expect("a valid document"),
    )
}

#[test]
fn invalid_goal_terms_exit_2_and_leave_a_record() {
    let (code, stderr, doc) = refused(&["--goal-turns", "2", "--goal-check", "", "do it"], "");
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("[sven:error] --goal-check: a goal check takes a command"),
        "{stderr}"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
}

#[test]
fn a_goal_combined_with_a_workflow_feature_exits_2_and_leaves_a_record() {
    let (code, stderr, doc) = refused(&["--goal-turns", "2", "--var", "a=b", "do it"], "");
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("[sven:error] --goal-turns runs a single prompt"),
        "{stderr}"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
}

#[test]
fn a_piped_conversation_without_a_pending_task_exits_2_and_leaves_a_record() {
    let (code, stderr, doc) = refused(&["--stdin"], "## User\nhi\n\n## Sven\nhello\n");
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("[sven:error] Piped conversation has no pending task."),
        "{stderr}"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
}

#[test]
fn an_unreadable_system_prompt_file_exits_2_and_leaves_a_record() {
    let (code, stderr, doc) = refused(&["--system-prompt-file", "missing.md", "do it"], "");
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("[sven:error] Failed to read --system-prompt-file"),
        "{stderr}"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
}

#[test]
fn a_trace_that_cannot_be_loaded_exits_2_and_leaves_a_record_elsewhere() {
    let (code, stderr, doc) = refused(&["--load-trace", "corrupt.json", "do it"], "");
    assert_eq!(code, Some(2));
    assert!(
        stderr.contains("[sven:error] Failed to load --load-trace"),
        "{stderr}"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "startup_error");
}

/// `--trace FILE` continues FILE: a failure to load it must not replace it.
#[test]
fn a_trace_that_is_also_the_output_is_not_overwritten_by_the_failure_record() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("t.json");
    std::fs::write(&trace, "{not json").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sven"))
        .args(["--headless", "--model", "mock", "--state-root"])
        .arg(dir.path().join("root"))
        .arg("--trace")
        .arg(&trace)
        .arg("do it")
        .current_dir(dir.path())
        .env_clear()
        .env("HOME", dir.path())
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), "{not json");
}
