// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//! Spec: what this harness derives is what the trainer actually accepts.
//!
//! The derivation writes `generic-messages-v2`, and every rule about that
//! format is enforced on the other side of a repository boundary by a parser
//! this code cannot see. A unit test asserting "the JSON looks right" asserts
//! this harness's *belief* about the format, and a belief is exactly what
//! drifts: the first sign of a mismatch would otherwise be a training run that
//! refuses a dataset after the GPU has been claimed, or worse, one that
//! accepts it and supervises the wrong spans.
//!
//! So the check runs the real parser. The lab links brain directly, which is
//! the whole reason `samples/learning/` is a separate workspace, and this is
//! what that buys.

use std::collections::BTreeMap;

use sample_learning_lab::{record_from_episode, to_jsonl, PredicateSet, Provenance, Verdict};
use sven_sdk::{ToolCallRecord, Turn};

fn solved() -> Verdict {
    let set = PredicateSet::new(["done"]).expect("one predicate");
    let observed: BTreeMap<String, bool> = [("done".to_string(), true)].into_iter().collect();
    set.evaluate(&observed).expect("evaluated")
}

/// An episode shaped like a real one: a system prompt, the request, a tool
/// call, its result, and a closing turn.
fn transcript() -> Vec<Turn> {
    vec![
        Turn::System {
            text: "You are an agent working in a repository.".into(),
        },
        Turn::User {
            text: "Enable 3 retry attempts for the service this host runs.".into(),
        },
        Turn::Assistant {
            text: String::new(),
            tool_calls: vec![ToolCallRecord {
                id: "call_0".into(),
                name: "shell".into(),
                arguments: r#"{"command":"./svctl status"}"#.into(),
            }],
        },
        Turn::ToolResult {
            call_id: "call_0".into(),
            text: "active deployment: staging".into(),
        },
        Turn::Assistant {
            text: "staging is live; editing config/staging.json".into(),
            tool_calls: vec![],
        },
    ]
}

#[test]
fn the_trainer_accepts_what_this_harness_derives() {
    let record = record_from_episode(
        "config-discovery",
        "staging",
        Provenance::OnPolicy,
        &transcript(),
        &solved(),
    )
    .expect("a verified episode yields a record");

    let jsonl = to_jsonl(&[record]).expect("serialises");
    let dir = std::env::temp_dir().join(format!("lab-trainable-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("train.jsonl");
    std::fs::write(&path, &jsonl).expect("write");

    // The real parser, from the crate that trains on it.
    let summary = brain::validate_chat_dataset(&path)
        .unwrap_or_else(|e| panic!("the trainer rejected a derived dataset: {e}\n{jsonl}"));

    assert_eq!(summary.records, 1);
    // system, user, assistant(tool call), tool result, assistant(text).
    assert_eq!(
        summary.messages, 5,
        "every turn the model saw must survive into the record: {jsonl}"
    );
    assert_eq!(
        summary.trained_messages, 2,
        "exactly the assistant turns are supervised, no more and no fewer: {jsonl}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_record_the_trainer_would_reject_never_gets_written() {
    // The derivation refuses an unverified episode, so the malformed-input
    // case the parser guards against cannot be produced here in the first
    // place. Belt and braces: assert the refusal, then assert that what does
    // get through parses.
    let set = PredicateSet::new(["done"]).expect("one predicate");
    let observed: BTreeMap<String, bool> = [("done".to_string(), false)].into_iter().collect();
    let unsolved = set.evaluate(&observed).expect("evaluated");

    assert!(
        record_from_episode(
            "f",
            "staging",
            Provenance::OnPolicy,
            &transcript(),
            &unsolved
        )
        .is_err(),
        "an unsolved episode must not reach the trainer at all"
    );
}
