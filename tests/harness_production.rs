// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run --toolset coding`: the real binary, the real coding
//! tools, a real C compiler and a loopback model server that plays a script.
//!
//! The specs: a run on real tools repairs a real repository and finishes only
//! when the independent gate passes; the model's key reaches the model server
//! and nothing else the run writes; the three caps (the harness's declared
//! capabilities, the agent step's allowlist, the host ceiling) and the
//! approval mode bound what the agent does; a gate the agent edits ends the
//! run; and no flag that widens a run is implied.
//!
//! Swedish Embedded AB implements governed autonomous coding workflows for
//! its clients. If your team needs expertise in letting agents change real
//! repositories under verifiable control then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod harness_support;

use std::path::{Path, PathBuf};
use std::process::Command;

use harness_support::{
    code, report, run, sample, serve, setup, shell, stderr, write, Setup, Turn, KEY, MODEL,
};
use serde_json::{json, Value};

const TEST_PROGRAM: &str = "#include <stdio.h>\n#include \"../src/ringbuf.h\"\nint main(void) { ringbuf rb; int v; rb_init(&rb); printf(\"pop on empty: %d\\n\", rb_pop(&rb, &v)); return rb_pop(&rb, &v) == -1 ? 0 : 1; }\n";

const FIRST_FIX: &str = include_str!("../tests/fixtures/harness/c-repair/solution/ringbuf.c");

/// The solution with the pop modulus bug put back: the first, partial fix.
fn partial_fix() -> String {
    FIRST_FIX.replace(
        "(rb->head + 1) % RB_CAPACITY",
        "(rb->head + 1) % (RB_CAPACITY - 1)",
    )
}

/// The turns of an agent that writes its test, builds and runs it with the
/// real compiler, makes a partial fix, and stops - then, after the gate
/// fails, completes the fix.
fn repairing_script() -> Vec<Turn> {
    vec![
        Turn::Calls(vec![write("tests/test_ringbuf.c", TEST_PROGRAM)]),
        Turn::Calls(vec![shell("mkdir -p build && cc -std=c99 -Wall -Wextra -Werror -o build/t tests/test_ringbuf.c src/ringbuf.c && ./build/t")]),
        Turn::Calls(vec![write("src/ringbuf.c", &partial_fix())]),
        Turn::Text("fixed the full check"),
        Turn::Calls(vec![write("src/ringbuf.c", FIRST_FIX)]),
        Turn::Text("fixed the pop wrap-around"),
    ]
}

/// Every file the run left outside the repository, as one string.
fn everything_written(setup: &Setup) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(setup.scratch.path()).unwrap().flatten() {
        if entry.file_name() != "keys.json" {
            text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        }
    }
    let audit = setup.repo.path().join(".sven/audit.jsonl");
    text.push_str(&std::fs::read_to_string(audit).unwrap_or_default());
    text
}

#[test]
fn a_coding_run_repairs_a_c_repository_and_finishes_only_when_the_gate_passes() {
    let setup = setup();
    let server = serve(repairing_script());
    let out = run(setup
        .command(&server)
        .arg("--approvals")
        .arg("auto")
        .arg("--trace")
        .arg(setup.out("trace.jsonl"))
        .arg("--record")
        .arg(setup.out("recording.json"))
        .arg("--trajectory")
        .arg(setup.out("trajectory.json")));
    let stderr_text = stderr(&out);
    assert_eq!(code(&out), 0, "{stderr_text}");

    let report = report(&out);
    assert_eq!(report["status"], "completed", "{report}");
    assert_eq!(
        report["output"],
        json!({"summary": "fixed the pop wrap-around"})
    );
    assert_eq!(
        report["step_counts"]["implement__v3_independent_test_a0_repair"], 1,
        "the first run of the gate failed: {report}"
    );
    assert!(
        report["step_counts"]
            .get("implement__v0_pinned_script_a1_fail")
            .is_none(),
        "the gate files were intact: {report}"
    );

    let repo = setup.repo.path();
    assert!(
        repo.join("tests/test_ringbuf.c").is_file(),
        "the agent's test exists"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("src/ringbuf.c")).unwrap(),
        FIRST_FIX,
        "the source was repaired on disk"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join(".gate/result.json"))
            .unwrap()
            .trim(),
        r#"{"passed": true}"#,
        "the gate recorded its own verdict"
    );

    let seen = server.seen.lock().unwrap();
    assert!(seen.requests >= 6, "the model was asked for every turn");
    assert!(
        seen.authorization
            .iter()
            .all(|a| a == &format!("Bearer {KEY}")),
        "the key reached the model server: {:?}",
        seen.authorization
    );
    let leaked = format!(
        "{}{}{}",
        String::from_utf8_lossy(&out.stdout),
        stderr_text,
        everything_written(&setup)
    );
    assert!(!leaked.contains(KEY), "the key is in nothing the run wrote");
}

#[test]
fn approvals_deny_refuses_every_write_and_every_shell_call_and_the_run_does_not_succeed() {
    let setup = setup();
    let server = serve(repairing_script());
    let out = run(setup.command(&server).args(["--approvals", "deny"]));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    // The harness's own gate step is a shell call too, so it is refused as
    // well: the run fails instead of passing without its gate.
    assert_eq!(report(&out)["failure"]["code"], "side_effect_failed");
    let source = std::fs::read_to_string(setup.repo.path().join("src/ringbuf.c")).unwrap();
    assert!(
        source.contains("count > RB_CAPACITY"),
        "no write got through"
    );
    assert!(!setup.repo.path().join("tests/test_ringbuf.c").exists());
    assert!(stderr(&out).contains("denied"), "{}", stderr(&out));
}

#[test]
fn the_host_ceiling_narrows_what_the_harness_declares() {
    let setup = setup();
    let server = serve(repairing_script());
    // The harness declares ReadFile, WriteFile, ExecuteShell and RunVerifier;
    // the host grants only reads and verifiers.
    let out = run(setup.command(&server).args([
        "--approvals",
        "auto",
        "--allow-capability",
        "ReadFile",
        "--allow-capability",
        "RunVerifier",
    ]));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let source = std::fs::read_to_string(setup.repo.path().join("src/ringbuf.c")).unwrap();
    assert!(
        source.contains("count > RB_CAPACITY"),
        "the host never granted WriteFile"
    );
    assert!(!setup.repo.path().join("tests/test_ringbuf.c").exists());
}

#[test]
fn an_agent_that_edits_the_gate_never_passes_it() {
    let setup = setup();
    let server = serve(vec![
        Turn::Calls(vec![write(
            ".gate/gate_test.c",
            "int main(void) { return 0; }\n",
        )]),
        Turn::Text("done"),
        Turn::Text("still done"),
    ]);
    let out = run(setup.command(&server).args(["--approvals", "auto"]));
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let report = report(&out);
    assert_eq!(report["failure"]["code"], "gate_failed", "{report}");
    assert!(
        report["failure"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("pinned_test")),
        "the failure names the pinned file: {report}"
    );
}

#[test]
fn flags_that_widen_a_run_are_never_implied() {
    let setup = setup();
    let server = serve(vec![]);
    let refused = |args: &[&str], needle: &str| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
        cmd.current_dir(setup.scratch.path())
            .env("HOME", setup.scratch.path())
            .args(["harness", "run"])
            .arg(sample())
            .args(args);
        let out = run(&mut cmd);
        assert_eq!(code(&out), 2, "{args:?}: {}", stderr(&out));
        assert!(stderr(&out).contains(needle), "{args:?}: {}", stderr(&out));
    };
    let ws = setup.repo.path().to_str().unwrap().to_string();
    let endpoint = server.base.clone();
    refused(&["--toolset", "coding"], "--workspace");
    refused(&["--toolset", "coding", "--workspace", &ws], "--model");
    refused(
        &["--toolset", "coding", "--workspace", &ws, "--model", MODEL],
        "--model-endpoint",
    );
    refused(
        &[
            "--toolset",
            "coding",
            "--workspace",
            &ws,
            "--model",
            MODEL,
            "--model-endpoint",
            &endpoint,
        ],
        "--approvals",
    );
    refused(
        &[
            "--toolset",
            "coding",
            "--workspace",
            &ws,
            "--model",
            MODEL,
            "--model-endpoint",
            &endpoint,
            "--approvals",
            "auto",
            "--model-fixture",
            "x.yaml",
        ],
        "fixture",
    );
    // An approval mode or a key means nothing on the fixture path.
    refused(&["--approvals", "auto"], "--toolset coding");
    refused(&["--allow-capability", "ReadFile"], "--toolset coding");
}

#[test]
fn a_key_file_inside_the_workspace_is_refused() {
    let setup = setup();
    let server = serve(vec![]);
    let inside = setup.repo.path().join("key.txt");
    std::fs::write(&inside, KEY).unwrap();
    let out = run(Command::new(env!("CARGO_BIN_EXE_sven"))
        .current_dir(setup.scratch.path())
        .env("HOME", setup.scratch.path())
        .args(["harness", "run"])
        .arg(sample())
        .args(["--toolset", "coding", "--approvals", "auto", "--workspace"])
        .arg(setup.repo.path())
        .args([
            "--model",
            MODEL,
            "--model-endpoint",
            &server.base,
            "--api-key-file",
        ])
        .arg(&inside));
    assert_eq!(code(&out), 2);
    assert!(
        stderr(&out).contains("inside the workspace"),
        "{}",
        stderr(&out)
    );
    assert!(!stderr(&out).contains(KEY));
}

// ── v2 workflows: the gate tool is on every host ──────────────────────────

fn workflow() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/harness/verified-v2")
}

/// A workspace holding the check, and `ok.txt` when `ready`.
fn workflow_repo(ready: bool) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("check.sh"), "test -f ok.txt\n").unwrap();
    if ready {
        std::fs::write(repo.path().join("ok.txt"), "ok\n").unwrap();
    }
    repo
}

fn none_command(scratch: &Path, repo: &Path, replies: &[&str]) -> Command {
    let model = scratch.join("model.yaml");
    let sequence: Vec<Value> = replies.iter().map(|t| json!({ "text": t })).collect();
    std::fs::write(&model, json!({ "sequence": sequence }).to_string()).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(scratch)
        .env("HOME", scratch)
        .env("XDG_CONFIG_HOME", scratch)
        .args(["harness", "run"])
        .arg(workflow())
        .args(["--toolset", "none", "--workspace"])
        .arg(repo)
        .arg("--model-fixture")
        .arg(&model)
        .args(["--json", "--timeout", "60"]);
    cmd
}

#[test]
fn a_workflow_gate_runs_a_real_command_with_the_none_toolset() {
    let scratch = tempfile::tempdir().unwrap();
    // The check passes: the exit status of the real command is 0.
    let out = run(&mut none_command(
        scratch.path(),
        workflow_repo(true).path(),
        &["done"],
    ));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(report(&out)["output"], json!({ "note": "done" }));

    // The agent claims it is fixed, but the command exits 1 both times: the
    // claim is not believed and the run fails on the gate.
    let out = run(&mut none_command(
        scratch.path(),
        workflow_repo(false).path(),
        &["all fixed", "really fixed"],
    ));
    let report = report(&out);
    assert_ne!(code(&out), 0, "{report}");
    assert_eq!(report["status"], "failed", "{report}");
    assert_eq!(report["failure"]["code"], "gate_failed", "{report}");
}

#[test]
fn a_workflow_gate_runs_a_real_command_with_the_coding_toolset() {
    let setup = setup();
    std::fs::remove_dir_all(setup.repo.path()).unwrap();
    std::fs::create_dir_all(setup.repo.path()).unwrap();
    std::fs::write(setup.repo.path().join("check.sh"), "test -f ok.txt\n").unwrap();
    let server = serve(vec![
        Turn::Calls(vec![write("ok.txt", "ok\n")]),
        Turn::Text("created ok.txt"),
    ]);
    let mut cmd = setup.command(&server);
    cmd.args(["--approvals", "auto"]);
    // Point the same command at the workflow instead of the C sample.
    let args: Vec<std::ffi::OsString> = cmd
        .get_args()
        .map(|a| {
            if Path::new(a) == sample() {
                workflow().into_os_string()
            } else {
                a.to_os_string()
            }
        })
        .collect();
    let mut swapped = Command::new(env!("CARGO_BIN_EXE_sven"));
    swapped
        .current_dir(setup.scratch.path())
        .env("HOME", setup.scratch.path())
        .env("XDG_CONFIG_HOME", setup.scratch.path())
        .env_remove("SVEN_MODEL")
        .args(args);
    let out = run(&mut swapped);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(report(&out)["output"], json!({ "note": "created ok.txt" }));
}
