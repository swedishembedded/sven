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

use harness_support::cli::fixtures;
use harness_support::{code, run, serve, setup, stderr, write, Turn};
use serde_json::Value;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_millis() as u64
}

#[test]
fn every_trace_record_the_cli_writes_or_prints_carries_the_time_it_observed_it() {
    let setup = setup();
    let server = serve(vec![]);
    let trace = setup.out("trace.jsonl");
    let before = now_ms();
    let out = run(setup
        .subcommand("run", &fixtures().join("decide"), &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"plan": "pro"}"#,
            "--trace",
        ])
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
        .map(|l| serde_json::from_str::<Value>(l).expect("a JSON line"))
        // The tool-call reports of a long step are not trace records.
        .filter(|l| !matches!(l["event"].as_str(), Some("tool_started" | "tool_finished")))
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
fn conversations_writes_one_line_per_agent_conversation_in_one_file() {
    let setup = setup();
    let server = serve(vec![
        Turn::Calls(vec![write("lexer.txt", "ok")]),
        Turn::Text("done lexer"),
        Turn::Calls(vec![write("parser.txt", "ok")]),
        Turn::Text("done parser"),
    ]);
    let file = setup.out("conversations.jsonl");
    let out = run(setup
        .subcommand("run", &fixtures().join("gated-sweep"), &server)
        .args([
            "--approvals",
            "auto",
            "--input",
            r#"{"modules": [{"name": "lexer"}, {"name": "parser"}]}"#,
            "--quiet",
            "--conversations",
        ])
        .arg(&file));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    // One file, one line per conversation: each module is worked on in a
    // conversation of its own.
    let lines: Vec<Value> = std::fs::read_to_string(&file)
        .expect("the file was written")
        .lines()
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect();
    let named: Vec<(u64, &str)> = lines
        .iter()
        .map(|l| (l["index"].as_u64().unwrap(), l["name"].as_str().unwrap()))
        .collect();
    assert_eq!(named, [(0, "notes#0"), (1, "notes#1")]);
    let first = &lines[0]["trajectory"];
    assert_eq!(first["extra"]["harness"]["conversation"], "notes#0");
    assert!(first["steps"].as_array().is_some_and(|s| !s.is_empty()));

    // The reader the SDK gives agrees.
    let read = sven_sdk::read_conversations(&file).expect("reads");
    assert_eq!(read.len(), 2);
    assert_eq!(read[1].name, "notes#1");
}

/// A directory is refused with a message: older versions wrote a file per
/// conversation there, and silently writing into it would mix the layouts.
#[test]
fn conversations_refuses_a_directory() {
    let setup = setup();
    let server = serve(vec![]);
    let dir = setup.out("conversations");
    std::fs::create_dir_all(&dir).unwrap();
    let out = run(setup
        .subcommand("run", &fixtures().join("gated-sweep"), &server)
        .args(["--approvals", "auto", "--quiet", "--conversations"])
        .arg(&dir));
    assert_ne!(code(&out), 0);
    assert!(stderr(&out).contains("JSON-lines file"), "{}", stderr(&out));
}
