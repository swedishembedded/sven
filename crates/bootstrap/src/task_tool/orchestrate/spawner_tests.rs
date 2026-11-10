// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: what a `task` result is read as. The strings are the ones
//! `isolation::settle` writes; this pins that the two stay in step.

use sven_tool_api::tool::ToolOutput;

use super::super::schedule::Status;
use super::read_outcome;
use crate::task_tool::isolation::{BRANCH_MARK, CONFLICT_MARK, FILES_HEADER};

#[test]
fn an_integrated_child_gives_its_words_files_and_tokens() {
    let out = ToolOutput::ok(
        "c",
        format!(
            "Sub-agent completed (success, stop=end_turn).\nHandle: h1\nDescription: d\n\n--- Result ---\nbuilt it\n\nIntegrated into your working tree (uncommitted). {FILES_HEADER}\n- a.py\n- pkg/b.py"
        ),
    );
    let got = read_outcome(&out, Some((10, 5)));
    assert_eq!(got.status, Status::Integrated);
    assert_eq!(got.report, "built it");
    assert_eq!(got.files_changed, ["a.py", "pkg/b.py"]);
    assert_eq!(got.tokens, Some((10, 5)));
}

#[test]
fn a_conflict_and_a_failure_are_told_apart_and_name_the_kept_branch() {
    let conflict = ToolOutput::err(
        "c",
        format!("work\n\n{CONFLICT_MARK} with your tree in: x. Nothing was applied. Its work is {BRANCH_MARK}sven/task-1' (inspect)."),
    );
    let got = read_outcome(&conflict, None);
    assert_eq!(
        (got.status, got.branch.as_deref()),
        (Status::Conflict, Some("sven/task-1"))
    );

    let failed = ToolOutput::err("c", format!("boom. Its work is {BRANCH_MARK}sven/task-2'."));
    let got = read_outcome(&failed, None);
    assert_eq!(
        (got.status, got.branch.as_deref()),
        (Status::Failed, Some("sven/task-2"))
    );
    assert!(got.files_changed.is_empty());
}

#[test]
fn a_long_report_is_cut() {
    let out = ToolOutput::ok("c", "x".repeat(10_000));
    assert!(read_outcome(&out, None).report.len() < 3100);
}
