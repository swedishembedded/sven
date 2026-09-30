// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: writers in separate processes extend one chain.
//!
//! Several sessions on one project share its audit log. Each flush takes an
//! exclusive lock on the log's sidecar and chains from the tip on disk, so
//! processes appending at the same time must leave one linear, verifiable
//! chain holding every record - not two forks, and nothing lost.
//!
//! The test runs this test binary again as each writer: a child sees
//! `SVEN_CHAIN_TEST_WRITER_LOG` and appends; in a normal run the writer test
//! finds no such variable and does nothing.

use std::path::Path;
use std::process::Command;

use sven_chain::{append_chain, verify_chain};

const WRITER_LOG: &str = "SVEN_CHAIN_TEST_WRITER_LOG";
const WRITERS: usize = 4;
const RECORDS_PER_WRITER: usize = 50;

#[test]
fn writer() {
    let Some(log) = std::env::var_os(WRITER_LOG) else {
        return;
    };
    let pid = std::process::id();
    for i in 0..RECORDS_PER_WRITER {
        // One record per flush, so the writers interleave as much as the
        // lock lets them.
        append_chain(
            Path::new(&log),
            vec![serde_json::json!({ "writer": pid, "i": i })],
        )
        .expect("an append succeeds");
    }
}

#[test]
fn writers_in_separate_processes_extend_one_chain() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let log = dir.path().join("audit.jsonl");
    let me = std::env::current_exe().expect("this test binary");

    let children: Vec<_> = (0..WRITERS)
        .map(|_| {
            Command::new(&me)
                .args(["--exact", "writer", "--test-threads", "1"])
                .env(WRITER_LOG, &log)
                .spawn()
                .expect("a writer process starts")
        })
        .collect();
    for mut child in children {
        assert!(child.wait().expect("a writer exits").success());
    }

    assert_eq!(
        verify_chain(&log).expect("one intact chain"),
        WRITERS * RECORDS_PER_WRITER,
        "every record from every process is in the chain"
    );
}
