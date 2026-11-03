// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The progress sink's contract: JSON is the unchanged `[sven:harness]` line,
//! text is a readable account that says only what the events say.

use serde_json::json;

use super::{Renderer, Style};

fn src(task: &str, role: &str, gate: Option<&str>, attempt: Option<u32>) -> serde_json::Value {
    let mut s = json!({"task": task, "role": role});
    if let Some(g) = gate {
        s["gate"] = json!(g);
    }
    if let Some(a) = attempt {
        s["attempt"] = json!(a);
    }
    s
}

/// A task whose first gate fails, is repaired and passes on the retry.
fn failed_gate_repair_and_pass() -> Vec<String> {
    let gate = |attempt, passed: bool, code: Option<i64>| {
        let mut e = json!({"event":"gate_evaluated","passed":passed,
            "kind": if passed {"passed"} else {"failed"},
            "source": src("interpret", "check", Some("spec_valid"), Some(attempt))});
        if let Some(c) = code {
            e["exit_code"] = json!(c);
        }
        e.to_string()
    };
    vec![
        json!({"event":"step_started","primitive":"agent","source":src("interpret","work",None,None)}).to_string(),
        json!({"event":"tool_started","tool":"read_file","args":"{\"path\":\"input/request.md\"}"}).to_string(),
        json!({"event":"tool_finished","tool":"read_file","error":false}).to_string(),
        json!({"event":"tool_started","tool":"write_file","args":"{\"path\":\"spec.json\",\"text\":\"{\\n  \\\"format"}).to_string(),
        json!({"event":"tool_finished","tool":"write_file","error":true}).to_string(),
        json!({"event":"verified","ok":true,"verifier":"file_exists","source":src("interpret","check",Some("spec.json"),Some(0))}).to_string(),
        json!({"event":"branch_selected","condition":true,"selected":"interpret__v1","step":"interpret__v0","source":src("interpret","check",Some("spec.json"),Some(0))}).to_string(),
        gate(0, false, Some(1)),
        json!({"event":"step_started","primitive":"agent","source":src("interpret","repair",Some("spec_valid"),Some(1))}).to_string(),
        json!({"event":"turn_retried","attempt":1,"reason":"empty_reply","step":"interpret__repair","source":src("interpret","repair",None,None)}).to_string(),
        gate(1, true, None),
        json!({"event":"step_started","primitive":"agent","source":src("review","work",None,None)}).to_string(),
        json!({"event":"failed","step":"review__work","code":"budget","message":"out of tokens"}).to_string(),
    ]
}

fn plain(lines: &[String]) -> Vec<String> {
    let mut r = Renderer::new(Style::Plain);
    lines.iter().flat_map(|l| r.render(l)).collect()
}

#[test]
fn text_follows_a_failed_gate_a_repair_and_the_passing_retry() {
    let expected = [
        "▸ interpret",
        "  ▸ work",
        "    read_file input/request.md",
        "    write_file spec.json",
        "    ✗ write_file failed",
        "  ✓ verify file_exists spec.json",
        "  → branch true: interpret__v1",
        "  ✗ gate spec_valid (exit 1)",
        "  ↻ repair 1",
        "  ↻ retry 1: empty_reply",
        "  ✓ gate spec_valid (after repair 1)",
        "▸ review",
        "  ▸ work",
        "  ✗ failed budget: out of tokens",
    ];
    assert_eq!(plain(&failed_gate_repair_and_pass()), expected);
}

#[test]
fn colour_wraps_symbols_only_and_plain_has_no_escapes() {
    let lines = failed_gate_repair_and_pass();
    assert!(plain(&lines).iter().all(|l| !l.contains('\x1b')));
    let mut r = Renderer::new(Style::Colour);
    let out: Vec<String> = lines.iter().flat_map(|l| r.render(l)).collect();
    assert!(
        out.iter().any(|l| l.contains("\x1b[31m")),
        "failures are red"
    );
    assert!(
        out.iter().any(|l| l.contains("\x1b[32m")),
        "passes are green"
    );
    let stripped: Vec<String> = out.iter().map(|l| strip(l)).collect();
    assert_eq!(stripped, plain(&lines));
}

fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for e in chars.by_ref() {
                if e == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn an_unknown_event_is_named_not_hidden_and_garbage_is_ignored() {
    let mut r = Renderer::new(Style::Plain);
    assert_eq!(r.render(r#"{"event":"brand_new"}"#), ["  · brand_new"]);
    assert!(r.render("not json").is_empty());
}
