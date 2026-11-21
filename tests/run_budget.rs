// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A run's budget: `agent.max_completion_tokens` and `agent.max_turns` bound
//! every model completion of a headless run, and a run that would need one
//! more than its budget allows ends with the typed budget outcome (exit 4,
//! `budget_exhausted` in the trajectory, one `[sven:error] ... budget
//! exhausted` line on stderr), never as an agent error.
//!
//! The runs use the scripted mock model, so no real model is involved.
//!
//! Swedish Embedded AB implements solutions for comparing agents at equal
//! token and turn budgets for its clients. If your team needs expertise in
//! budgeted evaluation of coding agents then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// The mock's script: `ping` is answered in one completion; `look around`
/// takes three: it loads the deferred `find_file` definition, calls it, and
/// replies once the result is in.
const RESPONSES: &str = r#"
responses:
  - match_type: equals
    pattern: "ping"
    reply: "pong"
  - match_type: equals
    pattern: "look around"
    tool_calls:
      - id: tc-1
        tool: find_file
        args: { pattern: "*.yaml" }
    after_tool_reply: "nothing here"
"#;

fn config(dir: &Path, agent: &str) -> std::path::PathBuf {
    let responses = dir.join("responses.yaml");
    std::fs::write(&responses, RESPONSES).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "model:\n  provider: mock\n  name: mock\n  mock_responses_file: {}\nagent:\n{agent}",
            responses.display()
        ),
    )
    .unwrap();
    config
}

fn sven(dir: &Path, agent: &str, prompt: &str) -> Command {
    let config = config(dir, agent);
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
        .args(["--headless", prompt]);
    cmd
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The line the examiner reads: `^\[sven:(?:agent:)?error\].*\bbudget exhausted\b`.
fn budget_lines(err: &str) -> Vec<&str> {
    err.lines()
        .filter(|l| l.starts_with("[sven:error]") || l.starts_with("[sven:agent:error]"))
        .filter(|l| {
            l.match_indices("budget exhausted").any(|(at, m)| {
                let before = l[..at].chars().next_back();
                let after = l[at + m.len()..].chars().next();
                !before.is_some_and(char::is_alphanumeric)
                    && !after.is_some_and(char::is_alphanumeric)
            })
        })
        .collect()
}

fn trajectory_outcome(path: &Path) -> String {
    let trace: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    trace["final_metrics"]["extra"]["outcome"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn a_run_that_needs_more_turns_than_its_cap_ends_with_the_budget_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("run.atif.json");
    let out = sven(dir.path(), "  max_turns: 1\n", "look around")
        .arg("--output-trace")
        .arg(&trace)
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(4), "{err}");
    assert_eq!(budget_lines(&err).len(), 1, "{err}");
    assert!(budget_lines(&err)[0].contains("1 of 1 turns"), "{err}");
    assert_eq!(trajectory_outcome(&trace), "budget_exhausted", "{err}");
}

#[test]
fn the_runtime_runner_concludes_the_same_way() {
    // Without --output-trace a single prompt runs on the kernel runner; the
    // trajectory it writes itself lands in .sven/logs.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".sven")).unwrap();
    let out = sven(dir.path(), "  max_turns: 1\n", "look around")
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(4), "{err}");
    assert_eq!(budget_lines(&err).len(), 1, "{err}");
}

#[test]
fn a_run_that_finishes_on_its_last_allowed_turn_is_a_normal_finish() {
    let dir = tempfile::tempdir().unwrap();
    let out = sven(dir.path(), "  max_turns: 3\n", "look around")
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    assert!(budget_lines(&err).is_empty(), "{err}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("nothing here"));
}

#[test]
fn a_completion_token_cap_is_in_force_on_every_recorded_request() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("requests.jsonl");
    let out = sven(dir.path(), "  max_completion_tokens: 50\n", "ping")
        .arg("--record-requests")
        .arg(&log)
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(0), "{err}");
    let requests: Vec<Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r["kind"] == "request")
        .collect();
    assert!(!requests.is_empty());
    for request in &requests {
        let limit = request["request"]["max_output_tokens_override"]
            .as_u64()
            .expect("the request carries an output limit");
        assert!(limit <= 50, "{request}");
        assert_eq!(
            request["run_budget"]["max_completion_tokens"], 50,
            "{request}"
        );
        assert_eq!(
            request["run_budget"]["completion_tokens_spent"], 0,
            "{request}"
        );
    }
}

#[test]
fn a_completion_whose_usage_is_unreported_is_charged_its_whole_allowance() {
    // The mock reports no usage for a tool call: the run cannot know what it
    // spent, so it is charged what it was allowed, says so, and stops.
    let dir = tempfile::tempdir().unwrap();
    let out = sven(dir.path(), "  max_completion_tokens: 1000\n", "look around")
        .output()
        .unwrap();
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(4), "{err}");
    assert!(err.contains("reported no usage"), "{err}");
    assert!(
        budget_lines(&err)[0].contains("1000 of 1000 completion tokens"),
        "{err}"
    );
}
