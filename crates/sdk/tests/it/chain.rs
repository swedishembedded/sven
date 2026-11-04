// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an application records what its agents did in the framework's own
//! hash-chained log, reached through the SDK alone.
//!
//! Swedish Embedded AB implements auditable agent runtimes for its clients.
//! If your team needs expertise in tamper-evident agent records then you can
//! procure our services by sending an email to info@swedishembedded.com.

use serde_json::json;
use sven_sdk::chain::{append_chain, read_chain, verify_chain};

#[test]
fn appended_records_form_a_chain_that_verifies_and_reads_back_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    append_chain(&log, vec![json!({"n": 1})]).unwrap();
    append_chain(&log, vec![json!({"n": 2}), json!({"n": 3})]).unwrap();
    assert_eq!(verify_chain(&log).unwrap(), 3);
    let seen: Vec<i64> = read_chain(&log)
        .unwrap()
        .iter()
        .map(|l| l.entry["n"].as_i64().unwrap())
        .collect();
    assert_eq!(seen, [1, 2, 3]);
}

#[test]
fn an_edited_record_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("events.jsonl");
    append_chain(&log, vec![json!({"n": 1}), json!({"n": 2})]).unwrap();
    let text = std::fs::read_to_string(&log)
        .unwrap()
        .replace("\"n\":1", "\"n\":9");
    std::fs::write(&log, text).unwrap();
    assert!(verify_chain(&log).is_err());
}
