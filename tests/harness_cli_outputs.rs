// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: what `sven harness run` writes besides its verdict - trace records
//! stamped with the time sven observed them, and each conversation of the run
//! as its own ATIF file for training data, only when asked for.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in testing
//! autonomous workflows without a live model then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod harness_support;

use harness_support::cli::{code, fixtures, run, sample, stderr, sven};
use serde_json::Value;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_millis() as u64
}

#[test]
fn every_trace_record_the_cli_writes_or_prints_carries_the_time_it_observed_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let trace = tmp.path().join("trace.jsonl");
    let before = now_ms();
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(fixtures().join("decide"))
        .args(["--input", r#"{"plan": "pro"}"#, "--trace"])
        .arg(&trace));
    let after = now_ms();
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    let stamps = |lines: Vec<Value>| -> Vec<u64> {
        assert!(lines.len() >= 2, "the run has a trace");
        lines
            .iter()
            .map(|l| {
                l["ts_ms"]
                    .as_u64()
                    .unwrap_or_else(|| panic!("ts_ms in {l}"))
            })
            .collect()
    };
    let written: Vec<Value> = std::fs::read_to_string(&trace)
        .expect("a trace file")
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect();
    let printed: Vec<Value> = stderr(&out)
        .lines()
        .filter_map(|l| l.strip_prefix("[sven:harness] "))
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect();
    assert_eq!(written.len(), printed.len());
    for (what, lines) in [("file", written), ("stderr", printed)] {
        let times = stamps(lines);
        assert!(
            times.windows(2).all(|w| w[0] <= w[1]),
            "{what}: observed in order: {times:?}"
        );
        assert!(
            times.iter().all(|t| (before..=after).contains(t)),
            "{what}: wall clock milliseconds during the run: {times:?} not in {before}..={after}"
        );
    }
}

#[test]
fn conversations_writes_one_trajectory_per_agent_conversation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("conversations");
    let sweep = sample("module-sweep");
    let out = run(sven(tmp.path())
        .arg("run")
        .arg(&sweep)
        .arg("--case")
        .arg(sweep.join("cases/sweeps-every-module.yaml"))
        .arg("--quiet")
        .arg("--conversations")
        .arg(&dir));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("the directory was written")
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    // The planning task, each module in its own conversation, the coverage pass.
    assert_eq!(
        names,
        [
            "0000-plan.json",
            "0001-modules-0.json",
            "0002-modules-1.json",
            "0003-coverage-1.json"
        ]
    );
    let first: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("0001-modules-0.json")).unwrap())
            .unwrap();
    assert_eq!(first["extra"]["harness"]["conversation"], "modules#0");
    assert!(first["steps"].as_array().is_some_and(|s| !s.is_empty()));
}
