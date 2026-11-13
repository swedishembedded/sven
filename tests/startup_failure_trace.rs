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
