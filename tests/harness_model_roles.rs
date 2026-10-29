// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run --model-role`: each role a workflow names is answered by
//! the model mapped to it, and a run whose roles are not mapped exactly is
//! refused before any model is asked.
//!
//! Swedish Embedded AB implements multi-model agent workflows for its
//! clients. If your team needs expertise in routing agent tasks to the right
//! model then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod harness_support;

use harness_support::cli::fixtures;
use harness_support::{code, report, run, serve, setup, stderr, Turn};

#[test]
fn each_role_is_answered_by_the_model_mapped_to_it() {
    let setup = setup();
    let server = serve(vec![Turn::Text("step one"), Turn::Text("done")]);
    let out = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"])
        .args(["--model-role", "planner=openai/scripted-planner"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(report(&out)["output"]["result"], "done");
    assert_eq!(
        server.seen.lock().unwrap().models,
        ["scripted-planner", "scripted-coder"],
        "the planner role's turn goes to its model, the unroled turn to --model"
    );
}

#[test]
fn an_unmapped_or_unknown_role_is_refused_before_any_model_is_asked() {
    let setup = setup();
    let server = serve(vec![]);
    let unmapped = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"]));
    assert_eq!(code(&unmapped), 2, "{}", stderr(&unmapped));
    assert!(
        stderr(&unmapped).contains("planner"),
        "{}",
        stderr(&unmapped)
    );

    let unknown = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"])
        .args(["--model-role", "planner=openai/scripted-planner"])
        .args(["--model-role", "planer=openai/typo"]));
    assert_eq!(code(&unknown), 2, "{}", stderr(&unknown));
    assert!(stderr(&unknown).contains("planer"), "{}", stderr(&unknown));
    assert_eq!(server.seen.lock().unwrap().requests, 0);
}

/// `--api-key-file` is read once, whatever the number of roles: a key fed
/// through a FIFO that holds it once (so nothing that runs later can read
/// it again) serves every role.
#[cfg(unix)]
#[test]
fn the_key_file_is_read_once_for_every_role() {
    let setup = setup();
    let fifo = setup.out("key.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let writer = {
        let fifo = fifo.clone();
        std::thread::spawn(move || {
            std::fs::write(&fifo, r#"{"openai": "sk-test-5f0c9d7e-not-a-real-key"}"#).unwrap()
        })
    };
    let server = serve(vec![Turn::Text("step one"), Turn::Text("done")]);
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(setup.scratch.path())
        .env("HOME", setup.scratch.path())
        .env("XDG_CONFIG_HOME", setup.scratch.path())
        .env("TMPDIR", setup.scratch.path())
        .args(["harness", "run"])
        .arg(fixtures().join("roles"))
        .arg("--workspace")
        .arg(setup.repo.path())
        .args([
            "--model",
            harness_support::MODEL,
            "--model-endpoint",
            &server.base,
        ])
        .args(["--model-role", "planner=openai/scripted-planner"])
        .args([
            "--approvals",
            "deny",
            "--json",
            "--timeout",
            "20",
            "--api-key-file",
        ])
        .arg(&fifo);
    // A regression would block on a second read: give it a deadline.
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the run blocked reading the key file a second time");
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(server.seen.lock().unwrap().requests, 2);
}
