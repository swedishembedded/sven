// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness`: the offline CLI over programmable harnesses.
//!
//! Every test runs the real binary with a tempdir as its working directory,
//! no model environment and a throwaway HOME, so nothing here can start,
//! contact or configure a model. The specs: exit codes mean what the help
//! says, `--json` stdout is pure JSON, files the run writes are usable by
//! the commands that read them, and a real model is refused without the
//! explicit opt-in.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness")
}

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples/harnesses")
        .join(name)
}

/// The binary, in `cwd`, with no model environment and no user config.
fn sven(cwd: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(cwd)
        .env("HOME", cwd)
        .env("XDG_CONFIG_HOME", cwd)
        .env_remove("SVEN_MODEL")
        .arg("harness");
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the sven binary runs")
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("exited normally")
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is pure JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

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
    assert_eq!(report["entry"], "check");
    assert!(report["plan_digest"]
        .as_str()
        .is_some_and(|d| !d.is_empty()));
    let steps: Vec<&str> = report["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(steps, ["check", "grant", "reject"]);

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
    assert!(schema["properties"]["steps"].is_object());

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
fn run_returns_json_and_exit_0() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .args(["--input", r#"{"plan": "pro"}"#, "--json"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["harness"], "decide");
    assert_eq!(report["status"], "completed");
    assert_eq!(report["output"]["tier"], "paid");
    assert_eq!(report["step_counts"]["check"], 1);
    assert!(report["steps"].as_u64().is_some_and(|n| n >= 2));
    assert!(report["plan_digest"].is_string());
    assert!(report["run_id"].is_string());
    assert!(report["usage"].is_object());
    assert!(
        stderr(&out).contains("[sven:harness]"),
        "progress goes to stderr: {}",
        stderr(&out)
    );

    let quiet = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .args(["--input", r#"{"plan": "pro"}"#, "--quiet"]));
    assert_eq!(code(&quiet), 0);
    assert!(!stderr(&quiet).contains("[sven:harness]"));
}

#[test]
fn run_takes_its_input_from_a_case_manifest_and_flags_override_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let case = fixtures().join("decide/cases/free.yaml");
    let from_case = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .arg("--case")
        .arg(&case)
        .arg("--json"));
    assert_eq!(code(&from_case), 1, "the case's free plan fails");

    let overridden = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .arg("--case")
        .arg(&case)
        .args(["--input", r#"{"plan": "pro"}"#, "--json"]));
    assert_eq!(code(&overridden), 0, "{}", stderr(&overridden));
    assert_eq!(stdout_json(&overridden)["output"]["tier"], "paid");
}

#[test]
fn a_fail_verdict_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .args(["--input", r#"{"plan": "free"}"#, "--json"]));
    assert_eq!(code(&out), 1);
    let report = stdout_json(&out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failure"]["code"], "not_eligible");
}

#[test]
fn bad_input_is_a_usage_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for input in ["not json", r#"{"plan": 7}"#, "{}"] {
        let out = run(sven(tmp.path())
            .arg("run")
            .arg(fixtures().join("decide"))
            .args(["--input", input]));
        assert_eq!(code(&out), 2, "{input}: {}", stderr(&out));
    }
}

#[test]
fn a_deadline_suspends_with_exit_3_and_writes_a_checkpoint_that_resume_completes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wait = fixtures().join("wait");
    let checkpoint = tmp.path().join("checkpoint.json");

    let out = run(sven(tmp.path())
        .arg("run")
        .arg(&wait)
        .arg("--tool-fixture")
        .arg(wait.join("blocked.tools.yaml"))
        .args(["--timeout", "1", "--json", "--checkpoint-out"])
        .arg(&checkpoint));
    assert_eq!(code(&out), 3, "{}", stderr(&out));
    assert_eq!(stdout_json(&out)["status"], "suspended");
    assert!(checkpoint.is_file(), "the bound left a checkpoint file");

    let out = run(sven(tmp.path())
        .arg("resume")
        .arg(&wait)
        .arg("--checkpoint")
        .arg(&checkpoint)
        .arg("--tool-fixture")
        .arg(wait.join("answered.tools.yaml"))
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["output"]["report"], "all green");
    assert_eq!(report["step_counts"]["call"], 2, "the read is re-entered");
}

#[test]
fn a_checkpoint_from_another_harness_is_refused_with_exit_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wait = fixtures().join("wait");
    let checkpoint = tmp.path().join("checkpoint.json");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(&wait)
        .arg("--tool-fixture")
        .arg(wait.join("blocked.tools.yaml"))
        .args(["--timeout", "1", "--checkpoint-out"])
        .arg(&checkpoint));
    assert_eq!(code(&out), 3, "{}", stderr(&out));

    let out = run(sven(tmp.path())
        .arg("resume")
        .arg(fixtures().join("decide"))
        .arg("--checkpoint")
        .arg(&checkpoint));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(!stderr(&out).is_empty(), "the refusal is explained");
}

#[test]
fn record_then_replay_matches_and_a_tampered_trace_exits_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let decide = fixtures().join("decide");
    let recording = tmp.path().join("run.json");
    let trace = tmp.path().join("trace.jsonl");

    let out = run(sven(tmp.path())
        .arg("run")
        .arg(&decide)
        .args(["--input", r#"{"plan": "pro"}"#, "--quiet", "--record"])
        .arg(&recording)
        .arg("--trace")
        .arg(&trace));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let lines = std::fs::read_to_string(&trace).expect("trace written");
    assert!(lines.lines().count() >= 2);
    for line in lines.lines() {
        serde_json::from_str::<Value>(line).expect("one JSON object per line");
    }

    let out = run(sven(tmp.path())
        .arg("replay")
        .arg(&decide)
        .arg("--recording")
        .arg(&recording)
        .arg("--expect-trace")
        .arg(&trace)
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout_json(&out)["output"]["tier"], "paid");

    let tampered = tmp.path().join("tampered.jsonl");
    std::fs::write(&tampered, lines.replacen("grant", "reject", 1)).expect("write");
    let out = run(sven(tmp.path())
        .arg("replay")
        .arg(&decide)
        .arg("--recording")
        .arg(&recording)
        .arg("--expect-trace")
        .arg(&tampered));
    assert_eq!(code(&out), 1, "{}", stderr(&out));

    let out = run(sven(tmp.path())
        .arg("replay")
        .arg(fixtures().join("wait"))
        .arg("--recording")
        .arg(&recording));
    assert_eq!(code(&out), 2, "a recording of another program is refused");
}

#[test]
fn test_runs_a_case_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("test")
        .arg(fixtures().join("decide"))
        .arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let suites = stdout_json(&out);
    assert_eq!(suites[0]["passed"], 2);
    assert_eq!(suites[0]["failed"], 0);

    let out = run(sven(tmp.path())
        .arg("test")
        .arg(fixtures().join("decide"))
        .args(["--case", "pro"]));
    assert_eq!(code(&out), 0);
    assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));

    let out = run(sven(tmp.path())
        .arg("test")
        .arg(fixtures().join("decide"))
        .args(["--case", "nonesuch"]));
    assert_eq!(code(&out), 2, "a case filter that matches nothing is usage");
}

#[test]
fn a_failing_case_makes_test_exit_1() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let copy = tmp.path().join("decide");
    copy_dir(&fixtures().join("decide"), &copy);
    std::fs::write(
        copy.join("cases/pro.yaml"),
        "input: { plan: pro }\nexpect: { status: completed, output: { tier: gold } }\n",
    )
    .expect("write");
    let out = run(sven(tmp.path()).arg("test").arg(&copy));
    assert_eq!(code(&out), 1);
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL pro"));
}

#[test]
fn the_committed_samples_pass_offline_and_the_live_case_is_skipped() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(sample(""))
        .expect("samples")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.join("cases").is_dir())
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 8, "the eight named samples: {dirs:?}");

    let out = run(sven(tmp.path()).arg("test").args(&dirs).arg("--json"));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let suites = stdout_json(&out);
    let suites = suites.as_array().expect("one report per directory");
    assert_eq!(suites.len(), 7);
    for (dir, suite) in dirs.iter().zip(suites) {
        assert_eq!(suite["failed"], 0, "{suite}");
        // Only ticket-routing's live-local case is skipped, and only it.
        let skipped = u64::from(dir.ends_with("ticket-routing"));
        assert_eq!(suite["skipped"].as_u64(), Some(skipped), "{suite}");
    }
}

#[test]
fn compare_reports_a_regression() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let variant = tmp.path().join("variant");
    copy_dir(&fixtures().join("decide"), &variant);
    let text = std::fs::read_to_string(variant.join("harness.yaml")).expect("read");
    std::fs::write(
        variant.join("harness.yaml"),
        text.replace("tier: paid", "tier: gold"),
    )
    .expect("write");
    let cases = fixtures().join("decide/cases");

    let compare = |extra: &[&str]| {
        run(sven(tmp.path())
            .arg("compare")
            .arg(fixtures().join("decide"))
            .arg(&variant)
            .arg("--cases")
            .arg(&cases)
            .args(extra))
    };
    let out = compare(&["--json"]);
    assert_eq!(code(&out), 0, "a report is not a failure: {}", stderr(&out));
    let report = stdout_json(&out);
    let changes: Vec<&str> = report["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .filter_map(|c| c["change"].as_str())
        .collect();
    assert!(changes.contains(&"regressed"), "{report}");
    assert_eq!(compare(&["--fail-on-regression"]).status.code(), Some(1));

    let out = run(sven(tmp.path())
        .arg("compare")
        .arg(fixtures().join("decide"))
        .arg(fixtures().join("broken.yaml"))
        .arg("--cases")
        .arg(&cases));
    assert_eq!(code(&out), 2, "an invalid variant is reported, exit 2");
}

#[test]
fn compare_with_seeds_pairs_runs_and_rejects_a_regression() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let variant = tmp.path().join("variant");
    copy_dir(&fixtures().join("decide"), &variant);
    let text = std::fs::read_to_string(variant.join("harness.yaml")).expect("read");
    std::fs::write(
        variant.join("harness.yaml"),
        text.replace("tier: paid", "tier: gold"),
    )
    .expect("write");
    let compare = |extra: &[&str]| {
        run(sven(tmp.path())
            .arg("compare")
            .arg(fixtures().join("decide"))
            .arg(&variant)
            .arg("--cases")
            .arg(fixtures().join("decide/cases"))
            .args(["--seed", "1", "--seed", "2", "--bootstrap-seed", "5"])
            .args(extra))
    };
    let out = compare(&["--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["decision"]["verdict"], "reject", "{report}");
    assert_eq!(report["regressions"], serde_json::json!(["pro"]));
    assert_eq!(report["seeds"], serde_json::json!([1, 2]));
    assert_eq!(report["cost"]["a"]["runs"], 4);
    assert_eq!(compare(&["--fail-on-regression"]).status.code(), Some(1));

    let out = run(sven(tmp.path())
        .arg("compare")
        .arg(fixtures().join("decide"))
        .arg(&variant)
        .arg("--cases")
        .arg(fixtures().join("decide/cases"))
        .args(["--seed", "1"]));
    assert_eq!(
        code(&out),
        2,
        "seeds need an explicit --bootstrap-seed: {}",
        stderr(&out)
    );
}

/// The sample bundle's `reshape` helper as a `.wasm` file in `dir`.
fn extract_sample_helper(dir: &Path) -> PathBuf {
    use base64::Engine as _;
    let bundle: Value = serde_yaml::from_str(
        &std::fs::read_to_string(sample("custom-helper").join("bundle.yaml")).expect("read"),
    )
    .expect("bundle yaml");
    let encoded = bundle["helpers"]["reshape"]["content"]
        .as_str()
        .expect("helper content");
    let path = dir.join("reshape.wasm");
    std::fs::write(
        &path,
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64"),
    )
    .expect("write wasm");
    path
}

#[test]
fn helper_run_executes_a_module_in_the_bounded_runtime() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wasm = extract_sample_helper(tmp.path());
    let out = run(sven(tmp.path())
        .args(["helper", "run"])
        .arg(&wasm)
        .args(["--input", r#"{"customer": "acme", "total": 3}"#]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout_json(&out),
        serde_json::json!({"customer": "acme", "total": 3}),
        "the sample helper is the identity"
    );

    let out = run(sven(tmp.path())
        .args(["helper", "run"])
        .arg(&wasm)
        .args(["--input", "{}", "--fuel", "1"]));
    assert_eq!(
        code(&out),
        1,
        "a helper out of fuel is a result, not a crash"
    );
    assert!(stderr(&out).contains("fuel"), "{}", stderr(&out));

    let out = run(sven(tmp.path())
        .args(["helper", "run"])
        .arg(tmp.path().join("missing.wasm"))
        .args(["--input", "{}"]));
    assert_eq!(code(&out), 2);
}

#[test]
fn bundle_composes_a_document_and_its_helpers_into_the_committed_sample_bundle() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let wasm = extract_sample_helper(tmp.path());
    let built = tmp.path().join("bundle.yaml");
    let out = run(sven(tmp.path())
        .arg("bundle")
        .arg(sample("custom-helper").join("document.yaml"))
        .arg("--helper")
        .arg(format!("reshape={}", wasm.display()))
        .arg("--output")
        .arg(&built));
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let parse = |path: &Path| -> Value {
        serde_yaml::from_str(&std::fs::read_to_string(path).expect("read")).expect("yaml")
    };
    assert_eq!(
        parse(&built),
        parse(&sample("custom-helper").join("bundle.yaml")),
        "the CLI builds what the sample ships"
    );
    let out = run(sven(tmp.path()).arg("validate").arg(&built));
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // A helper the document does not use, or one that is not WASM, is refused.
    let out = run(sven(tmp.path())
        .arg("bundle")
        .arg(sample("custom-helper").join("document.yaml"))
        .arg("--helper")
        .arg(format!("unused={}", wasm.display())));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    let junk = tmp.path().join("junk.wasm");
    std::fs::write(&junk, b"not wasm").expect("write");
    let out = run(sven(tmp.path())
        .arg("bundle")
        .arg(sample("custom-helper").join("document.yaml"))
        .arg("--helper")
        .arg(format!("reshape={}", junk.display())));
    assert_eq!(code(&out), 2, "{}", stderr(&out));
}

#[test]
fn run_without_a_model_source_fails_a_model_step_cleanly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(sample("ticket-routing"))
        .args(["--input", r#"{"ticket": "crash"}"#, "--json"]));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let report = stdout_json(&out);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failure"]["code"], "unroutable");
    assert!(
        stderr(&out).contains("no model source"),
        "stderr says what to pass: {}",
        stderr(&out)
    );
}

#[test]
fn a_real_model_is_refused_without_an_endpoint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(sample("ticket-routing"))
        .args([
            "--input",
            r#"{"ticket": "crash"}"#,
            "--model",
            "ollama/qwen3:8b",
        ]));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("--model-endpoint"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_live_opt_in_is_flags_only_and_the_old_variables_change_nothing() {
    // The retired variable names are assembled so that no source line in the
    // tree spells them: the removal is then provable with a plain grep.
    let retired = |tail: &str| format!("SVEN_{}_{tail}", "HARNESS");
    let tmp = tempfile::tempdir().expect("tempdir");

    let out = run(sven(tmp.path())
        .env(retired("LOCAL_MODEL"), "1")
        .env(retired("LOCAL_ENDPOINT"), "http://127.0.0.1:9")
        .arg("run")
        .arg(sample("ticket-routing"))
        .args([
            "--input",
            r#"{"ticket": "crash"}"#,
            "--model",
            "ollama/qwen3:8b",
        ]));
    assert_eq!(code(&out), 2, "the environment must not supply an endpoint");
    assert!(
        stderr(&out).contains("--model-endpoint"),
        "{}",
        stderr(&out)
    );

    let out = run(sven(tmp.path())
        .env(retired("LOCAL_MODEL"), "1")
        .env(retired("LOCAL_ENDPOINT"), "http://127.0.0.1:9")
        .arg("test")
        .arg(sample("ticket-routing"))
        .args(["--case", "live-local", "--json"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let suites = stdout_json(&out);
    assert_eq!(suites[0]["skipped"], 1, "{suites}");
    assert_eq!(suites[0]["passed"], 0, "{suites}");
}

#[test]
fn test_and_compare_take_the_live_flags_and_the_model_needs_its_endpoint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for sub in ["test", "compare"] {
        let out = run(sven(tmp.path()).arg(sub).arg("--help"));
        let help = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(help.contains("--model-endpoint"), "{sub}: {help}");
        assert!(help.contains("--model "), "{sub}: {help}");
    }
    let out = run(sven(tmp.path())
        .arg("test")
        .arg(sample("ticket-routing"))
        .args(["--model", "ollama/qwen3:8b"]));
    assert_eq!(code(&out), 2, "--model alone is a usage error");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).expect("copy");
        }
    }
}
