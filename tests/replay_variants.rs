// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven replay --variant`: a recorded request is sent again with a named
//! prompt section and a tool description swapped, N times, and each sample is
//! one JSON line saying what the model decided.
//!
//! The binary runs against a loopback chat-completions server that plays a
//! script, so no real model is involved.
//!
//! Swedish Embedded AB implements solutions for measuring how a prompt change
//! alters an LLM agent's next decision, in minutes instead of whole benchmark
//! runs, for its clients. If your team needs expertise in prompt and tool
//! definition tuning for coding agents then you can procure our services by
//! sending an email to info@swedishembedded.com.

mod harness_support;

use std::path::Path;
use std::process::Command;

use harness_support::{serve, Turn};
use serde_json::{json, Value};
use sven_model::manifest::PromptSections;
use sven_model::{CompletionRequest, Message, PromptManifest, ToolSchema};
use sven_model_drivers::request_log::Record;

/// A log with one recorded request: a three-section system prompt and a
/// `probe` tool that requires a `path`.
fn write_log(dir: &Path) -> std::path::PathBuf {
    let text = "identity\n\nmode text\n\nTools: use them.";
    let sections = PromptSections::new(
        text,
        &[
            ("identity".into(), 0..8),
            ("mode".into(), 8..19),
            ("tools".into(), 19..text.len()),
        ],
    );
    let messages = vec![Message::system(text), Message::user("go")];
    let tools = vec![ToolSchema {
        name: "probe".into(),
        description: "Probe.".into(),
        parameters: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        ..ToolSchema::default()
    }];
    let request = CompletionRequest {
        manifest: PromptManifest::describe(&messages, Some(&sections), &tools, None),
        messages,
        tools,
        stream: true,
        ..CompletionRequest::default()
    };
    let record = Record::Request {
        seq: 1,
        provider: "openai".into(),
        model: "scripted".into(),
        request: Box::new(request),
    };
    let file = dir.join("run.requests.jsonl");
    std::fs::write(
        &file,
        format!("{}\n", serde_json::to_string(&record).unwrap()),
    )
    .unwrap();
    file
}

fn sven(dir: &Path, base: &str) -> Command {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!("model:\n  provider: openai\n  name: scripted\n  api_key: sk-test\n  base_url: {base}\n"),
    )
    .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_sven"));
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env_remove("SVEN_MODEL")
        .arg("--config")
        .arg(config)
        .arg("replay");
    cmd
}

#[test]
fn list_names_the_sections_and_counts_the_tools() {
    let dir = tempfile::tempdir().unwrap();
    let log = write_log(dir.path());
    let server = serve(vec![]);
    let out = sven(dir.path(), &server.base)
        .arg(&log)
        .arg("--list")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("identity#") && text.contains("mode#") && text.contains("tools#"),
        "{text}"
    );
    assert!(text.contains("| tools=1"), "{text}");
}

#[test]
fn a_variant_is_replayed_n_times_and_each_sample_is_a_json_line() {
    let dir = tempfile::tempdir().unwrap();
    let log = write_log(dir.path());
    let variant = dir.path().join("terse.yaml");
    std::fs::write(
        &variant,
        "system_sections:\n  - {id: mode, replace: 'Be brief.'}\n\
         tools:\n  - {name: probe, description: 'Probe a path.'}\n",
    )
    .unwrap();
    // The model gets the first sample right and the second wrong.
    let server = serve(vec![
        Turn::Calls(vec![("probe", json!({"path": "a"}))]),
        Turn::Calls(vec![("probe", json!({"depth": 2}))]),
    ]);
    let out = sven(dir.path(), &server.base)
        .arg(&log)
        .args(["--variant"])
        .arg(&variant)
        .args(["--repeat", "2", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("every stdout line is JSON"))
        .collect();
    assert_eq!(lines.len(), 2);
    // The server answers concurrently-safe in order, but do not rely on which
    // sample got which turn.
    lines.sort_by_key(|l| l["calls"][0]["valid"] == json!(false));
    assert_eq!(
        (lines[0]["variant"].as_str(), lines[0]["request"].as_u64()),
        (Some("terse"), Some(1))
    );
    assert_eq!(lines[0]["calls"][0]["name"], "probe");
    assert_eq!(lines[0]["calls"][0]["arguments"], json!(["path"]));
    assert_eq!(lines[0]["calls"][0]["valid"], true);
    assert_eq!(lines[1]["calls"][0]["arguments"], json!(["depth"]));
    assert_eq!(lines[1]["calls"][0]["valid"], false);
    let samples: Vec<u64> = lines
        .iter()
        .map(|l| l["sample"].as_u64().unwrap())
        .collect();
    assert!(samples.contains(&1) && samples.contains(&2), "{samples:?}");
}

#[test]
fn a_variant_naming_a_section_the_request_lacks_is_refused_with_the_choices() {
    let dir = tempfile::tempdir().unwrap();
    let log = write_log(dir.path());
    let variant = dir.path().join("bad.yaml");
    std::fs::write(&variant, "system_sections: [{id: nope, remove: true}]\n").unwrap();
    let server = serve(vec![]);
    let out = sven(dir.path(), &server.base)
        .arg(&log)
        .arg("--variant")
        .arg(&variant)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("identity, mode, tools"), "{err}");
}
