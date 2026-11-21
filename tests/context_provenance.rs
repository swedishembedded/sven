// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Context provenance and forking without a context block: every completion
//! of a trajectory lists the context blocks it saw by name and digest, and a
//! session started from a recorded trajectory with `--drop-context-block`
//! sends no request that carries the dropped block's text.
//!
//! The runs use the scripted mock model, so no real model is involved.
//!
//! Swedish Embedded AB implements solutions for tracing which context each
//! decision of an LLM agent was made in, for its clients. If your team needs
//! expertise in agent trajectory provenance then you can procure our services
//! by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const HINT: &str = "HINT-7f3a: the answer is in src/lib.rs";

fn sven(dir: &Path, requests: &Path) -> Command {
    let responses = dir.join("responses.yaml");
    std::fs::write(
        &responses,
        format!(
            "request_log: {}\nresponses:\n  - match_type: default\n    reply: done\n",
            requests.display()
        ),
    )
    .unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "model:\n  provider: mock\n  name: mock\n  mock_responses_file: {}\n",
            responses.display()
        ),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env("XDG_RUNTIME_DIR", dir)
        .env("TMPDIR", dir)
        .env_remove("SVEN_MODEL")
        .env_remove("SVEN_MOCK_RESPONSES")
        .arg("--config")
        .arg(config)
        .arg("--headless")
        .args(["--append-system-prompt", HINT]);
    cmd
}

fn run(cmd: &mut Command) {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every request the mock model was sent, as it logged them.
fn requests(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// The `context_blocks` of every completion recorded in a trajectory.
fn context_blocks(trace: &Path) -> Vec<Vec<(String, String)>> {
    let trace: Value = serde_json::from_str(&std::fs::read_to_string(trace).unwrap()).unwrap();
    trace["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|step| step["metrics"]["extra"]["sven_ledger"].as_array())
        .flatten()
        .map(|record| {
            record["context_blocks"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .map(|b| {
                            (
                                b["name"].as_str().unwrap().to_string(),
                                b["sha256"].as_str().unwrap().to_string(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn recorded_run(dir: &Path) -> (PathBuf, PathBuf) {
    let log = dir.join("first.requests.jsonl");
    let trace = dir.join("first.atif.json");
    run(sven(dir, &log)
        .arg("--output-trace")
        .arg(&trace)
        .arg("look at the code"));
    (log, trace)
}

#[test]
fn every_completion_lists_the_context_blocks_it_saw() {
    let dir = tempfile::tempdir().unwrap();
    let (log, trace) = recorded_run(dir.path());
    assert!(requests(&log).iter().all(|r| r.contains(HINT)));
    let completions = context_blocks(&trace);
    assert!(
        !completions.is_empty(),
        "the trajectory records its completions"
    );
    for blocks in &completions {
        let names: Vec<&str> = blocks.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"system/identity"), "{names:?}");
        assert!(names.contains(&"system/append"), "{names:?}");
        assert!(blocks.iter().all(|(_, sha)| sha.len() == 64), "{blocks:?}");
    }
}

#[test]
fn a_fork_without_a_named_block_never_sends_its_text() {
    let dir = tempfile::tempdir().unwrap();
    let (_, recorded) = recorded_run(dir.path());

    let log = dir.path().join("fork.requests.jsonl");
    let trace = dir.path().join("fork.atif.json");
    run(sven(dir.path(), &log)
        .arg("--load-trace")
        .arg(&recorded)
        .args(["--drop-context-block", "system/append"])
        .arg("--output-trace")
        .arg(&trace)
        .arg("continue"));
    let sent = requests(&log);
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|r| !r.contains(HINT)), "{sent:?}");
    // The forked session still starts from the recorded conversation.
    assert!(sent.iter().all(|r| r.contains("look at the code")));
    // The fork's trajectory carries the recorded steps as they were; its own
    // completion, the last, saw no `system/append`.
    let completions = context_blocks(&trace);
    let own = completions.last().expect("the fork made a completion");
    assert!(
        own.iter().any(|(name, _)| name == "system/identity"),
        "{own:?}"
    );
    assert!(
        own.iter().all(|(name, _)| name != "system/append"),
        "{own:?}"
    );
}

#[test]
fn a_block_that_cannot_be_dropped_is_refused_before_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("requests.jsonl");
    let out = sven(dir.path(), &log)
        .args(["--drop-context-block", "system/hint"])
        .arg("go")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("system/append"), "{err}");
    assert!(!log.exists(), "no request was sent");
}
