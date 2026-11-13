// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements unattended agent runs that leave a complete
// record even when they are killed, for its clients. If your team needs
// expertise in agent pipelines you can procure our services by sending an
// email to info@swedishembedded.com.
//! Spec: a headless run that is told to terminate writes the trajectory it
//! has so far, stamped as interrupted, before it exits.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

#[test]
fn a_terminated_run_writes_its_trajectory_so_far() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace.json");
    let mut child = Command::new(env!("CARGO_BIN_EXE_sven"))
        .args([
            "--headless",
            "--model",
            "mock",
            "--output-format",
            "json",
            "--output-trace",
        ])
        .arg(&trace)
        .arg("run a slow command")
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env(
            "SVEN_MOCK_RESPONSES",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/mock_responses.yaml"
            ),
        )
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts");

    // The tool call is in flight (`sleep 60`) once its line is printed.
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(stderr.read_line(&mut line).unwrap() > 0, "sven ended early");
        if line.contains("[sven:tool:call]") {
            break;
        }
    }
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let code = child.wait().unwrap();

    assert_eq!(
        code.code(),
        Some(130),
        "an interrupted run exits 130: {code:?}"
    );
    let written = std::fs::read_to_string(&trace).expect("the trajectory was written");
    let doc: serde_json::Value = serde_json::from_str(&written).expect("a valid document");
    assert!(
        written.contains("run a slow command"),
        "the prompt is in the partial trajectory"
    );
    assert_eq!(doc["final_metrics"]["extra"]["outcome"], "cancelled");
}

/// Runs the mock agent headless with `extra` flags and returns the exit code
/// and the trajectory it left at `--output-trace`.
fn run_with_trace(prompt: &str, extra: &[&str]) -> (Option<i32>, serde_json::Value) {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace.json");
    let out = Command::new(env!("CARGO_BIN_EXE_sven"))
        .args(["--headless", "--model", "mock", "--output-trace"])
        .arg(&trace)
        .args(extra)
        .arg(prompt)
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env(
            "SVEN_MOCK_RESPONSES",
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/mock_responses.yaml"
            ),
        )
        .env("XDG_RUNTIME_DIR", dir.path())
        .stdin(Stdio::null())
        .output()
        .expect("sven runs");
    let written = std::fs::read_to_string(&trace)
        .unwrap_or_else(|e| panic!("no trajectory after exit {:?}: {e}", out.status));
    (
        out.status.code(),
        serde_json::from_str(&written).expect("a valid document"),
    )
}

#[test]
fn a_run_that_times_out_still_writes_its_trajectory() {
    let (code, doc) = run_with_trace("run a slow command", &["--run-timeout", "2"]);
    assert_eq!(code, Some(124));
    assert!(doc["steps"].as_array().is_some_and(|s| !s.is_empty()));
}

#[test]
fn a_run_that_exhausts_its_token_budget_still_writes_its_trajectory() {
    let (code, doc) = run_with_trace("run ls", &["--max-tokens", "1"]);
    assert!(code.is_some_and(|c| c != 0), "{code:?}");
    assert!(doc["steps"].as_array().is_some_and(|s| !s.is_empty()));
}
