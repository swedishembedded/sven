// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness`: the command line over programmable harnesses.
//!
//! Every test runs the real binary with a throwaway HOME and no model
//! environment. Runs go to a loopback chat-completions server that plays a
//! script, so a run's model is always one the test controls. The specs: exit
//! codes mean what the help says, `--json` stdout is pure JSON, files a run
//! writes are usable by the commands that read them, every sample is a valid
//! harness, and the environment supplies no model.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod harness_support;

use harness_support::cli::{code, fixtures, run, sample, stderr, stdout_json, sven};
use harness_support::{report, serve, setup, write, Turn};
use serde_json::{json, Value};

#[test]
fn validate_reports_valid_and_invalid_with_exit_codes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let good = fixtures().join("decide");
    let bad = fixtures().join("broken.yaml");

    let out = run(sven(tmp.path()).arg("validate").arg(&good).arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout_json(&out)[0]["valid"], true);

    let out = run(sven(tmp.path())
        .arg("validate")
        .arg(&good)
        .arg(&bad)
        .arg("--json"));
    assert_eq!(code(&out), 1, "one invalid harness fails the command");
    let reports = stdout_json(&out);
    assert_eq!(reports[0]["valid"], true);
    assert_eq!(reports[1]["valid"], false);
    assert!(
        reports[1]["errors"][0]
            .as_str()
            .is_some_and(|e| e.contains("nowhere")),
        "{reports}"
    );

    let out = run(sven(tmp.path())
        .arg("validate")
        .arg(tmp.path().join("missing")));
    assert_eq!(code(&out), 2, "an unreadable path is an IO error");
}

#[test]
fn inspect_json_lists_steps_and_digest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(fixtures().join("decide"))
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["name"], "decide");
    assert_eq!(report["entry"], "check__work");
    assert!(report["plan_digest"]
        .as_str()
        .is_some_and(|d| !d.is_empty()));
    let steps: Vec<&str> = report["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(
        steps,
        [
            "check__v0_eligible_a0",
            "check__v0_eligible_a0_fail",
            "check__work",
            "complete"
        ]
    );

    let out = run(sven(tmp.path())
        .arg("inspect")
        .arg(fixtures().join("broken.yaml")));
    assert_eq!(code(&out), 1, "an invalid harness is exit 1");
}

#[test]
fn schema_prints_the_document_schema() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path()).arg("schema"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let schema = stdout_json(&out);
    assert!(schema["$schema"].is_string(), "{schema}");
    assert!(schema["properties"]["workflow"].is_object());

    let out = run(sven(tmp.path())
        .arg("schema")
        .arg(fixtures().join("decide")));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let contract = stdout_json(&out);
    assert!(
        contract.to_string().contains("plan"),
        "the contract names the input: {contract}"
    );
}

#[test]
fn every_sample_is_a_valid_harness() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut samples: Vec<_> = std::fs::read_dir(sample(""))
        .expect("samples")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("harness.yaml").is_file())
        .collect();
    samples.sort();
    assert_eq!(samples.len(), 11, "the named samples: {samples:?}");

    let out = run(sven(tmp.path())
        .arg("validate")
        .args(&samples)
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    for (dir, report) in samples.iter().zip(stdout_json(&out).as_array().unwrap()) {
        assert_eq!(report["valid"], true, "{}: {report}", dir.display());
    }
}

#[test]
fn run_returns_json_and_exit_0() {
    let setup = setup();
    let server = serve(vec![]);
    let out = run(setup
        .subcommand("run", &fixtures().join("decide"), &server)
        .args(["--approvals", "auto", "--input", r#"{"plan": "pro"}"#]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["harness"], "decide");
    assert_eq!(report["status"], "completed");
    assert_eq!(report["output"]["tier"], "paid");
    assert_eq!(report["step_counts"]["check__v0_eligible_a0"], 1);
    assert!(report["steps"].as_u64().is_some_and(|n| n >= 2));
    assert!(report["plan_digest"].is_string());
    assert!(report["run_id"].is_string());
    for key in ["input_tokens", "cached_tokens", "output_tokens", "cost_usd"] {
        assert!(report["usage"].get(key).is_some(), "usage reports {key}");
    }
    assert!(
        stderr(&out).contains("[sven:harness]"),
        "progress goes to stderr: {}",
        stderr(&out)
    );
    assert_eq!(
        server.seen.lock().unwrap().requests,
        0,
        "a harness without a model task never asks the model"
    );

    let quiet = run(setup
        .subcommand("run", &fixtures().join("decide"), &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"plan": "pro"}"#,
            "--quiet",
        ]));
    assert_eq!(code(&quiet), 0);
    assert!(!stderr(&quiet).contains("[sven:harness]"));
}

#[test]
fn run_names_its_scratchpad_on_stderr_or_in_the_json_report() {
    let setup = setup();
    let server = serve(vec![]);
    let pad = setup.out("pad");
    let named = run(setup
        .subcommand("run", &fixtures().join("decide"), &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"plan": "pro"}"#,
            "--scratchpad",
        ])
        .arg(&pad));
    assert_eq!(code(&named), 0, "{}", stderr(&named));
    let real = pad.canonicalize().expect("the directory was created");
    assert_eq!(
        report(&named)["scratchpad"],
        real.to_string_lossy().as_ref()
    );
    assert!(real.join("audit.jsonl").is_file(), "the audit log is in it");

    // Without --json the verdict is on stdout and the path on stderr.
    let mut human = harness_support::cli::sven(setup.scratch.path());
    human
        .arg("run")
        .arg(fixtures().join("decide"))
        .arg("--workspace")
        .arg(setup.repo.path())
        .args([
            "--model",
            harness_support::MODEL,
            "--model-endpoint",
            &server.base,
        ])
        .arg("--api-key-file")
        .arg(setup.key_file())
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"plan": "pro"}"#,
            "--scratchpad",
        ])
        .arg(&pad);
    let human = run(&mut human);
    assert!(
        stderr(&human).contains(&format!("scratchpad: {}", real.display())),
        "{}",
        stderr(&human)
    );
    assert!(
        !String::from_utf8_lossy(&human.stdout).contains("scratchpad"),
        "stdout keeps to the verdict"
    );
}

#[test]
fn a_fail_verdict_exits_1() {
    let setup = setup();
    let server = serve(vec![]);
    let out = run(setup
        .subcommand("run", &fixtures().join("decide"), &server)
        .args(["--approvals", "auto", "--input", r#"{"plan": "free"}"#]));
    assert_eq!(code(&out), 1);
    let report = report(&out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failure"]["code"], "gate_failed");
}

#[test]
fn bad_input_is_a_usage_error() {
    let setup = setup();
    let server = serve(vec![]);
    for input in ["not json", r#"{"plan": 7}"#, "{}"] {
        let out = run(setup
            .subcommand("run", &fixtures().join("decide"), &server)
            .args(["--approvals", "auto", "--input", input]));
        assert_eq!(code(&out), 2, "{input}: {}", stderr(&out));
    }
}

#[cfg(unix)]
#[test]
fn a_deadline_suspends_with_exit_3_and_writes_a_checkpoint_that_resume_completes() {
    let setup = setup();
    let checkpoint =
        harness_support::suspended_checkpoint(&setup, &fixtures().join("coding-resume"), "{}");

    let answering = serve(vec![
        Turn::Calls(vec![write("notes.txt", "hello")]),
        Turn::Text("done"),
    ]);
    let out = run(setup
        .subcommand("resume", &fixtures().join("coding-resume"), &answering)
        .args(["--approvals", "auto", "--checkpoint"])
        .arg(&checkpoint));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["output"], json!({"status": "written"}));
}

#[cfg(unix)]
#[test]
fn a_checkpoint_from_another_harness_is_refused_with_exit_2() {
    let setup = setup();
    let checkpoint =
        harness_support::suspended_checkpoint(&setup, &fixtures().join("coding-resume"), "{}");
    let server = serve(vec![]);
    let out = run(setup
        .subcommand("resume", &fixtures().join("decide"), &server)
        .args(["--approvals", "auto", "--checkpoint"])
        .arg(&checkpoint));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(!stderr(&out).is_empty(), "the refusal is explained");
}

#[test]
fn record_then_replay_matches_and_a_tampered_trace_exits_1() {
    let setup = setup();
    let server = serve(vec![]);
    let decide = fixtures().join("decide");
    let recording = setup.out("run.json");
    let trace = setup.out("trace.jsonl");

    let out = run(setup
        .subcommand("run", &decide, &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"plan": "pro"}"#,
            "--quiet",
            "--record",
        ])
        .arg(&recording)
        .arg("--trace")
        .arg(&trace));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let lines = std::fs::read_to_string(&trace).expect("trace written");
    assert!(lines.lines().count() >= 2);
    for line in lines.lines() {
        serde_json::from_str::<Value>(line).expect("one JSON object per line");
    }

    let tmp = setup.scratch.path();
    let out = run(sven(tmp)
        .arg("replay")
        .arg(&decide)
        .arg("--recording")
        .arg(&recording)
        .arg("--expect-trace")
        .arg(&trace)
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout_json(&out)["output"]["tier"], "paid");

    let tampered = tmp.join("tampered.jsonl");
    std::fs::write(&tampered, lines.replacen("check__work", "tampered", 1)).expect("write");
    let out = run(sven(tmp)
        .arg("replay")
        .arg(&decide)
        .arg("--recording")
        .arg(&recording)
        .arg("--expect-trace")
        .arg(&tampered));
    assert_eq!(code(&out), 1, "{}", stderr(&out));

    let out = run(sven(tmp)
        .arg("replay")
        .arg(fixtures().join("coding-resume"))
        .arg("--recording")
        .arg(&recording));
    assert_eq!(code(&out), 2, "a recording of another program is refused");
}

#[test]
fn the_environment_supplies_no_model_endpoint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let setup = setup();
    let server = serve(vec![]);
    let decide = fixtures().join("decide");
    let workspace = setup.repo.path();

    // A model without its endpoint is refused; the environment supplies none.
    // The retired variable names are assembled so that no source line in the
    // tree spells them: their removal is then provable with a plain grep.
    let retired = |tail: &str| format!("SVEN_{}_{tail}", "HARNESS");
    let out = run(sven(tmp.path())
        .env(retired("LOCAL_MODEL"), "1")
        .env(retired("LOCAL_ENDPOINT"), &server.base)
        .arg("run")
        .arg(&decide)
        .arg("--workspace")
        .arg(workspace)
        .args(["--approvals", "auto", "--model", "ollama/qwen3:8b"]));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("--model-endpoint"),
        "{}",
        stderr(&out)
    );

    assert_eq!(server.seen.lock().unwrap().requests, 0);
}

#[test]
fn a_trace_of_a_workflow_names_the_task_gate_item_and_result() {
    let setup = setup();
    let server = serve(vec![
        Turn::Calls(vec![write("lexer.txt", "ok")]),
        Turn::Text("done lexer"),
        Turn::Calls(vec![write("parser.txt", "ok")]),
        Turn::Text("done parser"),
    ]);
    let trace = setup.out("trace.jsonl");
    let out = run(setup
        .subcommand("run", &fixtures().join("gated-sweep"), &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"modules": [{"name": "lexer"}, {"name": "parser"}]}"#,
            "--quiet",
            "--trace",
        ])
        .arg(&trace));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let lines: Vec<Value> = std::fs::read_to_string(&trace)
        .expect("a trace file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect();
    let gate = lines
        .iter()
        .find(|l| l["event"] == "gate_evaluated" && l["source"]["item_id"] == "lexer")
        .expect("the lexer's check is on the trace");
    assert_eq!(gate["source"]["task"], "notes");
    assert_eq!(gate["source"]["gate"], "written");
    assert_eq!(gate["source"]["result"], "passed");
    assert_eq!(gate["passed"], true, "the record's own fields are kept");
}
