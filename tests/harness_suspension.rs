// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run --toolset coding` under interruption: a stop that can be
//! caught leaves a checkpoint, a kill that cannot be caught loses at most the
//! item in flight, and a long step reports what it is doing as it goes.
//!
//! The specs: a SIGTERM during a model call suspends the run and `resume`
//! completes it; a SIGKILL during a sweep leaves the checkpoint of the last
//! completed item and resuming does not repeat it; tool calls and step starts
//! are on stderr while the step is still running.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod harness_support;

use std::path::{Path, PathBuf};
use std::process::Command;

use harness_support::{code, report, run, serve, setup, stderr, write, Turn, KEY};
use serde_json::json;

fn coding_resume() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness/coding-resume")
}

#[cfg(unix)]
#[test]
fn a_terminated_coding_run_leaves_a_checkpoint_that_resume_completes() {
    use std::process::Stdio;

    let setup = setup();
    let stalled = serve(vec![Turn::Stall]);
    let checkpoint = setup.out("checkpoint.json");
    let child = setup
        .subcommand("run", &coding_resume(), &stalled)
        .args(["--approvals", "auto", "--checkpoint-out"])
        .arg(&checkpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts");

    // The model call is in flight once the request has reached the
    // (stalled) server.
    while stalled.seen.lock().unwrap().requests == 0 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let out = child.wait_with_output().expect("sven exits");
    assert_eq!(
        code(&out),
        3,
        "a terminated run is suspended: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(report(&out)["status"], "suspended");
    assert!(checkpoint.is_file(), "the signal left a checkpoint");
    assert!(!setup.repo.path().join("notes.txt").exists());

    // Resume under a model that now answers.
    let answering = serve(vec![
        Turn::Calls(vec![write("notes.txt", "hello")]),
        Turn::Text("done"),
    ]);
    let out = run(setup
        .subcommand("resume", &coding_resume(), &answering)
        .args(["--approvals", "auto", "--checkpoint"])
        .arg(&checkpoint));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(report(&out)["output"], json!({"status": "written"}));
    assert_eq!(
        std::fs::read_to_string(setup.repo.path().join("notes.txt")).unwrap(),
        "hello"
    );
}

#[cfg(unix)]
#[test]
fn tool_calls_are_reported_while_the_next_model_turn_is_still_running() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let setup = setup();
    // The second model turn never answers, so only a live report can show
    // the first turn's tool call.
    let server = serve(vec![
        Turn::Calls(vec![write("notes.txt", "hello")]),
        Turn::Stall,
    ]);
    let mut child = setup
        .subcommand("run", &coding_resume(), &server)
        .args(["--approvals", "auto"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut seen = String::new();
    let mut line = String::new();
    while !seen.contains("\"tool_finished\"") {
        line.clear();
        if stderr.read_line(&mut line).unwrap() == 0 {
            break;
        }
        seen.push_str(&line);
    }
    child.kill().expect("sven is still running");
    child.wait().unwrap();
    assert!(
        seen.contains("\"tool_started\"") && seen.contains("write_file"),
        "the call was reported as it began: {seen}"
    );
    assert!(
        seen.contains("\"tool_finished\"") && seen.contains("\"error\":false"),
        "and as it ended: {seen}"
    );
    assert!(!seen.contains(KEY), "no report carries the key: {seen}");
}

fn coding_sweep() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness/coding-sweep")
}

const SWEEP_INPUT: &str = r#"{"modules": [{"name": "a"}, {"name": "b"}]}"#;

/// A run that is killed without a chance to react (SIGKILL, an OOM kill)
/// leaves the checkpoint of its last completed item, and resuming it does
/// not work on that item again.
#[cfg(unix)]
#[test]
fn a_killed_sweep_resumes_at_the_item_it_was_on() {
    use std::process::Stdio;

    let setup = setup();
    // Item a: write, then conclude. Item b: write, then the model stalls.
    let stalled = serve(vec![
        Turn::Calls(vec![write("a.txt", "ok")]),
        Turn::Text("done a"),
        Turn::Calls(vec![write("b.txt", "ok")]),
        Turn::Stall,
    ]);
    let checkpoint = setup.out("checkpoint.json");
    let mut child = setup
        .subcommand("run", &coding_sweep(), &stalled)
        .args([
            "--approvals",
            "auto",
            "--input",
            SWEEP_INPUT,
            "--checkpoint-out",
        ])
        .arg(&checkpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sven starts");
    while stalled.seen.lock().unwrap().requests < 4 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().expect("sven is still running");
    child.wait().unwrap();

    assert!(checkpoint.is_file(), "the completed item left a checkpoint");
    assert!(
        !checkpoint.with_extension("json.partial").exists(),
        "no half-written file is left behind"
    );
    let out = run(Command::new(env!("CARGO_BIN_EXE_sven"))
        .current_dir(setup.scratch.path())
        .env("HOME", setup.scratch.path())
        .args(["harness", "status"])
        .arg(coding_sweep())
        .arg("--checkpoint")
        .arg(&checkpoint)
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let status = report(&out);
    assert_eq!(status["loops"][0]["completed"], 1, "{status}");
    assert_eq!(status["loops"][0]["total"], 2, "{status}");

    // Resume under a model that now answers: only item b is worked on.
    let answering = serve(vec![Turn::Text("done b")]);
    let out = run(setup
        .subcommand("resume", &coding_sweep(), &answering)
        .args(["--approvals", "auto", "--checkpoint"])
        .arg(&checkpoint)
        .arg("--checkpoint-out")
        .arg(&checkpoint));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        answering.seen.lock().unwrap().requests,
        1,
        "item a was not worked on again"
    );
    assert!(
        !checkpoint.exists(),
        "a finished run leaves no checkpoint offering to resume it"
    );
}

/// A step is reported when it begins, not when it ends: with the model
/// stalled on its first turn, the step's start is already on stderr.
#[cfg(unix)]
#[test]
fn a_step_is_reported_as_started_while_its_model_turn_is_still_running() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let setup = setup();
    let stalled = serve(vec![Turn::Stall]);
    let mut child = setup
        .subcommand("run", &coding_resume(), &stalled)
        .args(["--approvals", "auto"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("sven starts");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    let mut started = false;
    while stderr.read_line(&mut line).unwrap() > 0 {
        if line.contains("\"step_started\"") && line.contains("\"work\"") {
            started = true;
            break;
        }
        line.clear();
    }
    child.kill().expect("sven is still running");
    child.wait().unwrap();
    assert!(started, "the work step's start was reported while it ran");
}
