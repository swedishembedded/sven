// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! Representations of a chunk at each visibility: what they keep, what they
//! state, and the floors that failures and pinned chunks never go below.

use sven_chunks::{allowed_visibilities, render, ChunkId, ChunkKind, RenderRequest, Visibility};

fn request<'a>(kind: ChunkKind, raw: &'a str) -> RenderRequest<'a> {
    RenderRequest {
        id: ChunkId::for_content(raw.as_bytes()),
        kind,
        source_tool: "shell",
        path: None,
        failure: false,
        pinned: false,
        raw,
    }
}

fn log_with_one_error() -> String {
    let mut lines: Vec<String> = (1..=300).map(|n| format!("compiling crate {n}")).collect();
    lines[149] = "error[E0308]: mismatched types".into();
    lines[210] = "warning: unused variable `x`".into();
    lines.join("\n")
}

#[test]
fn full_is_the_raw_text_unchanged() {
    let raw = log_with_one_error();
    let r = render(&request(ChunkKind::CommandOutput, &raw), Visibility::Full);
    assert_eq!(r.visibility, Visibility::Full);
    assert_eq!(r.text, raw);
}

#[test]
fn short_command_output_keeps_ends_and_error_lines_with_numbers_and_says_how_to_expand() {
    let raw = log_with_one_error();
    let req = request(ChunkKind::CommandOutput, &raw);
    let r = render(&req, Visibility::Short);
    assert_eq!(r.visibility, Visibility::Short);
    assert!(r.text.contains("compiling crate 1\n"), "first lines");
    assert!(r.text.contains("compiling crate 300"), "last lines");
    assert!(r.text.contains("L150: error[E0308]: mismatched types"));
    assert!(r.text.contains("L211: warning: unused variable"));
    assert!(
        r.text.contains("1 error") && r.text.contains("1 warning"),
        "counts: {}",
        r.text
    );
    assert!(r.text.contains("300 lines"));
    assert!(r.text.contains(req.id.as_str()), "names the chunk id");
    assert!(r.text.contains("omitted"), "states what was left out");
    assert!(r.text.len() < raw.len() / 3);
}

#[test]
fn shell_output_reports_its_exit_status() {
    let raw = format!("[exit 101]\n{}", log_with_one_error());
    let r = render(&request(ChunkKind::CommandOutput, &raw), Visibility::Short);
    assert!(r.text.contains("exit: 101"), "{}", r.text);
}

#[test]
fn short_diff_lists_files_and_hunk_headers_only() {
    let raw = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,4 @@ fn a\n ctx\n-old\n+new\n+newer\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -9 +9 @@\n-x\n+y\n";
    let r = render(&request(ChunkKind::Diff, raw), Visibility::Short);
    assert!(r.text.contains("src/a.rs") && r.text.contains("b.txt"));
    assert!(r.text.contains("@@ -1,3 +1,4 @@ fn a") && r.text.contains("@@ -9 +9 @@"));
    assert!(r.text.contains("+2 -1"), "per-file counts: {}", r.text);
    assert!(!r.text.contains("newer"), "no hunk bodies in Short");
}

#[test]
fn short_file_gives_path_size_and_an_outline() {
    let raw = "L1:use x;\nL2:pub struct Alpha {}\nL3:    fn inner() {}\nL4:pub fn beta() {}\nL5:impl Alpha {}\n";
    let mut req = request(ChunkKind::File, raw);
    req.source_tool = "read_file";
    req.path = Some("src/lib.rs");
    let r = render(&req, Visibility::Short);
    assert!(r.text.contains("src/lib.rs"));
    assert!(r.text.contains("5 lines"));
    assert!(r.text.contains("pub struct Alpha") && r.text.contains("pub fn beta"));
    assert!(r.text.contains(req.id.as_str()));
}

#[test]
fn long_is_larger_than_short_and_smaller_than_full() {
    let raw = log_with_one_error();
    let req = request(ChunkKind::CommandOutput, &raw);
    let short = render(&req, Visibility::Short).text.len();
    let long = render(&req, Visibility::Long).text.len();
    assert!(short < long && long < raw.len());
}

#[test]
fn hidden_is_a_reference_that_still_states_size_and_id() {
    let raw = log_with_one_error();
    let req = request(ChunkKind::CommandOutput, &raw);
    let r = render(&req, Visibility::Hidden);
    assert_eq!(r.visibility, Visibility::Hidden);
    assert!(r.text.contains(req.id.as_str()) && r.text.contains("300 lines"));
    assert!(r.text.len() < 300);
}

#[test]
fn a_failure_is_never_rendered_below_long() {
    let raw = log_with_one_error();
    let mut req = request(ChunkKind::CommandOutput, &raw);
    req.failure = true;
    for asked in [Visibility::Hidden, Visibility::Short] {
        let r = render(&req, asked);
        assert_eq!(r.visibility, Visibility::Long, "asked for {asked:?}");
    }
    assert_eq!(
        allowed_visibilities(true, false),
        vec![Visibility::Long, Visibility::Full]
    );
}

#[test]
fn a_pinned_chunk_is_always_full() {
    let raw = log_with_one_error();
    let mut req = request(ChunkKind::UserConstraint, &raw);
    req.pinned = true;
    assert_eq!(
        render(&req, Visibility::Hidden).visibility,
        Visibility::Full
    );
    assert_eq!(allowed_visibilities(false, true), vec![Visibility::Full]);
}

#[test]
fn an_ordinary_chunk_may_take_any_visibility() {
    assert_eq!(
        allowed_visibilities(false, false),
        vec![
            Visibility::Hidden,
            Visibility::Short,
            Visibility::Long,
            Visibility::Full
        ]
    );
}

#[test]
fn failure_is_recognised_from_the_text_a_tool_returns() {
    use sven_chunks::indicates_failure;
    assert!(indicates_failure("error: [exit 2]\nboom"));
    assert!(indicates_failure("[exit 1]\nno matches"));
    assert!(indicates_failure(
        "running 3 tests\ntest result: FAILED. 2 passed; 1 failed"
    ));
    assert!(!indicates_failure(
        "all good\ntest result: ok. 3 passed; 0 failed"
    ));
    assert!(!indicates_failure("fn handle_error() {}"));
}

#[test]
fn every_form_below_full_names_the_chunk_and_what_was_omitted_for_every_kind() {
    let raw = log_with_one_error();
    let kinds = [
        ChunkKind::File,
        ChunkKind::CommandOutput,
        ChunkKind::TestResult,
        ChunkKind::Diff,
        ChunkKind::UserConstraint,
        ChunkKind::Instruction,
        ChunkKind::SubagentFinding,
        ChunkKind::Conversation,
    ];
    for kind in kinds {
        for asked in [Visibility::Hidden, Visibility::Short, Visibility::Long] {
            for failure in [false, true] {
                let mut req = request(kind, &raw);
                req.failure = failure;
                let r = render(&req, asked);
                assert!(r.visibility >= asked);
                if failure {
                    assert!(r.visibility >= Visibility::Long, "{kind:?} {asked:?}");
                }
                assert!(r.text.contains(req.id.as_str()), "{kind:?} {asked:?}");
                assert!(
                    r.text.contains("tokens"),
                    "{kind:?} {asked:?} must state the size: {}",
                    r.text
                );
            }
        }
    }
}
