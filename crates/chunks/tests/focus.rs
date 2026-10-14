// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! What the shortened forms keep because the conversation points at it: the
//! lines a call or an error names, the definition of what the task is about,
//! the cause in front of an error and the lines that share its identifiers.

use sven_chunks::{render, ChunkId, ChunkKind, Focus, RenderRequest, Visibility};

fn request<'a>(
    kind: ChunkKind,
    tool: &'a str,
    path: Option<&'a str>,
    raw: &'a str,
    focus: Option<&'a Focus>,
) -> RenderRequest<'a> {
    RenderRequest {
        id: ChunkId::for_content(raw.as_bytes()),
        kind,
        source_tool: tool,
        path,
        failure: false,
        pinned: false,
        raw,
        focus,
        note: None,
    }
}

fn source_file() -> String {
    let mut lines: Vec<String> = Vec::new();
    for n in 1..=60 {
        lines.push(format!("pub fn helper_{n}(x: u64) -> u64 {{"));
        lines.push(format!("    x + {n} // body of helper {n}"));
        lines.push("}".to_string());
        lines.push(String::new());
    }
    lines.push("pub fn settle_ledger(entries: &[u64], end: u64) -> u64 {".into());
    lines.push("    let cutoff = end - 1; // CAUSE-LINE".into());
    lines.push("    entries.iter().filter(|e| **e < cutoff).sum() // EFFECT-LINE".into());
    lines.push("}".into());
    lines.join("\n")
}

fn focus_from(texts: &[&str]) -> Focus {
    let mut focus = Focus::default();
    for t in texts {
        focus.observe(t);
    }
    focus
}

// ── what the conversation points at ─────────────────────────────────────────

#[test]
fn a_focus_reads_line_mentions_and_named_identifiers_from_text() {
    let focus = focus_from(&[
        "see src/a.rs:42:7 and tests/b.rs:10-14, also `settle_ledger` and `x`",
        "error at lib/mod.py:3",
    ]);
    assert!(focus.mentions("src/a.rs", 42));
    assert!(focus.mentions("a.rs", 42), "a path may be given shorter");
    assert!(focus.mentions("tests/b.rs", 12), "a range covers its lines");
    assert!(!focus.mentions("tests/b.rs", 15));
    assert!(focus.mentions("lib/mod.py", 3));
    assert!(focus.names("settle_ledger"));
    assert!(!focus.names("x"), "too short to name anything");
}

#[test]
fn a_focus_is_bounded() {
    let mut focus = Focus::default();
    for n in 0..10_000 {
        focus.observe(&format!("src/f{n}.rs:{n} `ident_{n}`"));
    }
    assert!(focus.len() <= 512, "{}", focus.len());
}

// ── files ───────────────────────────────────────────────────────────────────

#[test]
fn a_short_file_keeps_the_lines_an_error_names_and_the_declaration_around_them() {
    let raw = source_file();
    let focus = focus_from(&["error: src/lib.rs:122:5 mismatched types"]);
    let r = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/lib.rs"),
            &raw,
            Some(&focus),
        ),
        Visibility::Short,
    );
    assert!(
        r.text.contains("L122:") && r.text.contains("body of helper 31"),
        "{}",
        r.text
    );
    assert!(
        r.text.contains("L121: pub fn helper_31") || r.text.contains("L121:"),
        "the line before is kept: {}",
        r.text
    );
    assert!(r.text.contains("omitted") && r.text.contains("expand"));
}

#[test]
fn a_short_file_keeps_the_declaration_around_a_named_line_even_beyond_the_outline_cap() {
    let raw = source_file();
    let focus = focus_from(&["panic at src/lib.rs:199"]);
    let r = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/lib.rs"),
            &raw,
            Some(&focus),
        ),
        Visibility::Short,
    );
    assert!(
        r.text.contains("pub fn helper_50("),
        "enclosing signature: {}",
        r.text
    );
}

#[test]
fn a_short_file_keeps_the_whole_definition_of_what_the_conversation_names() {
    let raw = source_file();
    let focus = focus_from(&["Find where `settle_ledger` is used."]);
    let r = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/lib.rs"),
            &raw,
            Some(&focus),
        ),
        Visibility::Short,
    );
    assert!(
        r.text.contains("CAUSE-LINE") && r.text.contains("EFFECT-LINE"),
        "{}",
        r.text
    );
    assert!(
        !r.text.contains("body of helper 30"),
        "other bodies stay out"
    );
}

#[test]
fn the_definition_of_a_named_function_brings_the_definitions_of_the_functions_it_calls_here() {
    let mut raw = source_file();
    raw.push_str("\n\npub fn settle_via_helper(entries: &[u64]) -> u64 {\n    inner_total(entries)\n}\n\nfn inner_total(entries: &[u64]) -> u64 {\n    entries.iter().sum() // INNER-BODY\n}\n");
    let focus = focus_from(&["Find where `settle_via_helper` is used."]);
    let r = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/lib.rs"),
            &raw,
            Some(&focus),
        ),
        Visibility::Short,
    );
    assert!(
        r.text.contains("INNER-BODY"),
        "one hop from the named function: {}",
        r.text
    );
    assert!(
        r.text.contains("inner_total (L"),
        "the note names it: {}",
        r.text
    );
    assert!(!r.text.contains("body of helper 30"));
}

#[test]
fn only_one_hop_of_callees_is_followed() {
    let raw = "pub fn named(x: u64) -> u64 {\n    first_step(x)\n}\nfn first_step(x: u64) -> u64 {\n    second_step(x)\n}\nfn second_step(x: u64) -> u64 {\n    x + 1 // TWO-BODY\n}\n".repeat(1);
    let focus = focus_from(&["`named`"]);
    let r = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/lib.rs"),
            raw.as_str(),
            Some(&focus),
        ),
        Visibility::Short,
    );
    assert!(
        r.text.contains("second_step(x)"),
        "the callee's body is kept"
    );
    assert!(
        !r.text.contains("TWO-BODY"),
        "its own callee is not: {}",
        r.text
    );
}

#[test]
fn a_file_that_only_mentions_the_name_in_a_comment_gets_no_definition() {
    let raw = "// see settle_ledger: deprecated\nfn other() {\n    1\n}\n".repeat(30);
    let focus = focus_from(&["`settle_ledger`"]);
    let with = render(
        &request(
            ChunkKind::File,
            "read_file",
            Some("src/n.rs"),
            &raw,
            Some(&focus),
        ),
        Visibility::Short,
    );
    let without = render(
        &request(ChunkKind::File, "read_file", Some("src/n.rs"), &raw, None),
        Visibility::Short,
    );
    assert_eq!(with.text, without.text);
}

// ── command output ──────────────────────────────────────────────────────────

fn build_log() -> String {
    let mut lines: Vec<String> = (1..=400)
        .map(|n| format!("2026-03-14T09:00:{:02}Z INFO step {n} took {n}ms", n % 60))
        .collect();
    // Benign lines that use the words, by level, before the real failures.
    for n in 0..60 {
        lines[10 + n] = format!("WARN retry succeeded after transient error on attempt {n}");
    }
    lines[100] = "INFO summary: 0 failed, 412 passed".into();
    lines[300] = "running tests::keeps_invariant_holds".into();
    lines[330] = "ERROR cache::evict invariant violated: bucket 5 over capacity".into();
    lines[331] = "FAILED tests::keeps_invariant_holds - panicked at src/lib.rs:88".into();
    lines[350] = "retrying tests::keeps_invariant_holds once".into();
    lines.join("\n")
}

#[test]
fn errors_outrank_look_alike_lines_when_the_cap_is_reached() {
    let raw = build_log();
    let r = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, None),
        Visibility::Short,
    );
    assert!(r.text.contains("invariant violated"), "{}", r.text);
    assert!(
        r.text.contains("FAILED tests::keeps_invariant_holds"),
        "{}",
        r.text
    );
}

#[test]
fn an_error_line_keeps_the_lines_just_before_it() {
    let raw = build_log();
    let r = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, None),
        Visibility::Short,
    );
    assert!(
        r.text.contains("INFO step 330 took"),
        "cause context: {}",
        r.text
    );
    assert!(r.text.contains("INFO step 329 took"), "{}", r.text);
}

#[test]
fn lines_that_share_an_identifier_with_an_error_are_kept() {
    let raw = build_log();
    let r = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, None),
        Visibility::Short,
    );
    assert!(
        r.text.contains("running tests::keeps_invariant_holds"),
        "{}",
        r.text
    );
    assert!(
        r.text
            .contains("retrying tests::keeps_invariant_holds once"),
        "{}",
        r.text
    );
}

#[test]
fn the_extra_lines_are_bounded() {
    let raw: String = (0..3000)
        .map(|n| format!("ERROR module::thing_{n} failed at src/f{}.rs:{n}\n", n % 7))
        .collect();
    let r = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, None),
        Visibility::Short,
    );
    assert!(
        r.text.lines().count() < 200,
        "{} lines",
        r.text.lines().count()
    );
}

// ── search results ──────────────────────────────────────────────────────────

#[test]
fn a_short_search_result_keeps_the_first_matches_of_every_file() {
    let raw: String = (0..40)
        .flat_map(|f| {
            (1..=6).map(move |m| format!("src/mod_{f:02}/file.rs:{}:9:match {f} {m}\n", m * 10))
        })
        .collect();
    let r = render(
        &request(ChunkKind::CommandOutput, "grep", Some("src"), &raw, None),
        Visibility::Short,
    );
    for f in 0..40 {
        assert!(
            r.text
                .contains(&format!("src/mod_{f:02}/file.rs:10:9:match {f} 1")),
            "file {f}: {}",
            r.text
        );
    }
    assert!(r.text.contains("40 files"), "{}", r.text);
    assert!(r.text.contains("omitted") && r.text.contains("expand"));
}

// ── invariants ──────────────────────────────────────────────────────────────

#[test]
fn a_focus_does_not_weaken_the_floors_or_the_statement_of_what_was_left_out() {
    let raw = source_file();
    let focus = focus_from(&["src/lib.rs:20 `settle_ledger`"]);
    let mut req = request(
        ChunkKind::File,
        "read_file",
        Some("src/lib.rs"),
        &raw,
        Some(&focus),
    );
    for v in [Visibility::Hidden, Visibility::Short, Visibility::Long] {
        let r = render(&req, v);
        assert!(
            r.text.contains(req.id.as_str()) && r.text.contains("expand"),
            "{v:?}"
        );
    }
    req.pinned = true;
    assert_eq!(render(&req, Visibility::Hidden).text, raw);
    req.pinned = false;
    req.failure = true;
    assert_eq!(
        render(&req, Visibility::Hidden).visibility,
        Visibility::Long
    );
}

#[test]
fn a_form_is_a_function_of_the_text_and_the_focus() {
    let raw = build_log();
    let focus = focus_from(&["src/lib.rs:88"]);
    let a = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, Some(&focus)),
        Visibility::Long,
    );
    let b = render(
        &request(ChunkKind::CommandOutput, "shell", None, &raw, Some(&focus)),
        Visibility::Long,
    );
    assert_eq!(a, b);
}
