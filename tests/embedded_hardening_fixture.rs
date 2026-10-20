// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the embedded-hardening sample's disposable firmware repository is
//! what the sample says it is. The hashes the workflow pins are the files'
//! hashes; the fixture starts red (failing tests, a HAL leak into a public
//! header) and the shipped reference fix turns every gate green with coverage
//! over the threshold, using the real compiler, gcov and clang-tidy.
//!
//! Swedish Embedded AB implements verifiable test-and-harden workflows for
//! embedded C codebases for its clients. If your team needs expertise in
//! agent-driven firmware hardening that is judged by the build and the tests,
//! not by the agent, then you can procure our services by sending an email to
//! info@swedishembedded.com.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/harnesses/embedded-hardening")
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn gate(repo: &Path, args: &[&str]) -> bool {
    Command::new("sh")
        .arg(".hardening/run.sh")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("sh runs")
        .status
        .success()
}

fn sha256(file: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(file)
        .output()
        .expect("sha256sum");
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn the_workflow_pins_the_hashes_of_the_check_and_of_the_given_tests() {
    let yaml = std::fs::read_to_string(sample().join("harness.yaml")).unwrap();
    for file in [".hardening/run.sh", ".hardening/pins.sha256"] {
        let hash = sha256(&sample().join("repo").join(file));
        assert!(yaml.contains(&hash), "harness.yaml pins {file} as {hash}");
    }
    // pins.sha256 lists the given tests; the fixture's build refuses a changed one.
    let repo = tempfile::tempdir().unwrap();
    copy_dir(&sample().join("repo"), repo.path());
    if have("cc") {
        assert!(gate(repo.path(), &["build"]));
        let test = repo.path().join("tests/test_crc8.c");
        std::fs::write(&test, "int main(void) { return 0; }\n").unwrap();
        assert!(
            !gate(repo.path(), &["build"]),
            "a weakened test is caught by its pin"
        );
    }
}

#[test]
fn the_fixture_starts_red_and_the_reference_fix_turns_every_gate_green() {
    if !(have("cc") && have("gcovr") && have("clang-tidy")) {
        eprintln!("skipped: needs cc, gcovr and clang-tidy");
        return;
    }
    let repo = tempfile::tempdir().unwrap();
    copy_dir(&sample().join("repo"), repo.path());
    let root = repo.path();
    for module in ["ringbuf", "crc8", "uart_tx"] {
        assert!(
            !gate(root, &["test", module]),
            "{module} starts with failing tests"
        );
    }
    assert!(
        !gate(root, &["layering"]),
        "the HAL leaks into a public header"
    );
    assert!(gate(root, &["lint"]));

    copy_dir(&sample().join("solution"), root);
    for module in ["ringbuf", "crc8", "uart_tx"] {
        assert!(
            gate(root, &["test", module]),
            "{module} passes after the fix"
        );
    }
    assert!(gate(root, &["build"]));
    assert!(gate(root, &["layering"]));
    assert!(gate(root, &["lint"]));
    assert!(gate(root, &["coverage"]));
    let figure: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".hardening/coverage.json")).unwrap(),
    )
    .unwrap();
    assert!(figure["line_pct"].as_f64().unwrap() >= 90.0, "{figure}");
}
