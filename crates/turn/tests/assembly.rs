// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The context assembly policy: which tool results a request carries in full,
//! which it demotes, and the invariants no advisor can break.

use std::collections::HashSet;

use sven_chunks::{ChunkId, Visibility, EXPAND_TOOL_NAME};
use sven_model::{FunctionCall, Message, MessageContent, Role, ToolResultContent};
use sven_turn::assembly::{
    Candidate, ContextPolicy, DeterministicAdvisor, PolicyConfig, VisibilityAdvisor,
};

fn call(id: &str, name: &str, args: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        },
    }
}

fn log(tag: &str, lines: usize) -> String {
    (1..=lines)
        .map(|n| format!("{tag} line {n} of output"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `rounds` rounds of one shell call each, every result `lines` long.
fn history(rounds: usize, lines: usize) -> Vec<Message> {
    let mut m = vec![Message::user("do the work")];
    for r in 0..rounds {
        let id = format!("call{r}");
        m.push(call(&id, "shell", &format!(r#"{{"command":"step {r}"}}"#)));
        m.push(Message::tool_result(&id, log(&format!("r{r}"), lines)));
        m.push(Message::assistant(format!("done step {r}")));
    }
    m
}

fn policy(budget: u32, keep: u32) -> ContextPolicy {
    ContextPolicy::new(PolicyConfig {
        budget_tokens: budget,
        keep_recent_rounds: keep,
        min_demote_tokens: 16,
    })
}

fn result_text(m: &Message) -> &str {
    match &m.content {
        MessageContent::ToolResult {
            content: ToolResultContent::Text(t),
            ..
        } => t,
        other => panic!("not a text tool result: {other:?}"),
    }
}

fn results(view: &[Message]) -> Vec<&str> {
    view.iter()
        .filter(|m| matches!(m.content, MessageContent::ToolResult { .. }))
        .map(result_text)
        .collect()
}

fn assemble(p: &ContextPolicy, messages: Vec<Message>) -> sven_turn::assembly::AssembledView {
    p.assemble(messages, &HashSet::new(), &DeterministicAdvisor)
}

#[test]
fn a_history_within_the_budget_is_returned_byte_for_byte() {
    let messages = history(4, 30);
    let view = assemble(&policy(1_000_000, 1), messages.clone());
    assert_eq!(
        serde_json::to_string(&view.messages).unwrap(),
        serde_json::to_string(&messages).unwrap()
    );
    assert!(view.demotions.is_empty() && view.spill.is_empty());
}

#[test]
fn over_budget_the_oldest_results_are_demoted_and_the_recent_rounds_stay_full() {
    let messages = history(6, 200);
    let original = results(&messages)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let view = assemble(&policy(3_000, 2), messages.clone());
    let now = results(&view.messages);
    assert_eq!(now[4], original[4], "second most recent round stays full");
    assert_eq!(now[5], original[5], "most recent round stays full");
    assert_ne!(now[0], original[0], "the oldest result is demoted");
    assert!(!view.demotions.is_empty());
    // Non-result messages are untouched.
    assert_eq!(view.messages.len(), messages.len());
    assert_eq!(view.messages[0].role, Role::User);
}

#[test]
fn demotion_stops_once_the_view_fits_the_budget_and_each_demotion_is_recorded() {
    let messages = history(6, 200);
    let before: usize = messages.iter().map(Message::approx_tokens).sum();
    let view = assemble(&policy(before as u32 - 600, 1), messages);
    let after: usize = view.messages.iter().map(Message::approx_tokens).sum();
    assert!(after as u32 <= before as u32 - 600, "fits the budget");
    assert!(view.demotions.len() < 5, "newer results are left alone");
    assert!(after < before);
    for d in &view.demotions {
        assert!(d.to < d.from && d.tokens_saved > 0);
        assert_eq!(d.tool, "shell");
    }
}

#[test]
fn a_demoted_result_names_its_chunk_and_the_raw_text_is_handed_back_for_storage() {
    let messages = history(4, 200);
    let raw0 = results(&messages)[0].to_string();
    let view = assemble(&policy(500, 1), messages);
    let shown = results(&view.messages)[0].to_string();
    let id = ChunkId::for_content(raw0.as_bytes());
    assert!(shown.contains(id.as_str()) && shown.contains(EXPAND_TOOL_NAME));
    let spill = view
        .spill
        .iter()
        .find(|e| e.raw == raw0)
        .expect("raw evidence of every demotion is returned");
    assert_eq!(spill.draft.provenance.source_tool, "shell");
    assert_eq!(
        view.demotions[0].chunk_id, id,
        "oldest result is demoted first"
    );
}

#[test]
fn a_failure_is_never_demoted_below_long_whatever_the_budget() {
    let mut messages = history(3, 200);
    let failing = format!(
        "[exit 101]\n{}\nerror[E0308]: mismatched types",
        log("f", 300)
    );
    messages[2] = Message::tool_result("call0", &failing);
    let view = assemble(&policy(0, 0), messages);
    let shown = results(&view.messages)[0];
    assert!(
        shown.contains("| long |"),
        "{}",
        &shown[..120.min(shown.len())]
    );
    assert!(shown.contains("error[E0308]: mismatched types"));
    let d = view
        .demotions
        .iter()
        .find(|d| d.chunk_id == ChunkId::for_content(failing.as_bytes()))
        .unwrap();
    assert!(d.to >= Visibility::Long);
}

#[test]
fn a_pinned_result_is_always_full() {
    let messages = history(3, 200);
    let raw0 = results(&messages)[0].to_string();
    let pinned: HashSet<ChunkId> = [ChunkId::for_content(raw0.as_bytes())].into();
    let view = policy(0, 0).assemble(messages, &pinned, &DeterministicAdvisor);
    assert_eq!(results(&view.messages)[0], raw0);
    assert!(view.demotions.iter().all(|d| d.tokens_saved > 0));
    assert_eq!(view.demotions.len(), 2);
}

#[test]
fn expansions_and_tools_without_a_representation_are_never_demoted() {
    let mut m = vec![Message::user("go")];
    m.push(call(
        "e1",
        EXPAND_TOOL_NAME,
        r#"{"chunk":"c-0123456789abcdef"}"#,
    ));
    m.push(Message::tool_result("e1", log("expanded", 400)));
    m.push(call("w1", "edit_file", r#"{"path":"a"}"#));
    m.push(Message::tool_result("w1", log("edit", 400)));
    m.push(call("s1", "shell", "{}"));
    m.push(Message::tool_result("s1", log("sh", 400)));
    let original = results(&m)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let view = assemble(&policy(0, 0), m);
    let now = results(&view.messages);
    assert_eq!(now[0], original[0]);
    assert_eq!(now[1], original[1]);
    assert_ne!(now[2], original[2]);
}

#[test]
fn small_results_are_not_worth_demoting() {
    let mut m = vec![Message::user("go")];
    m.push(call("s1", "shell", "{}"));
    m.push(Message::tool_result("s1", "ok"));
    let view = assemble(&policy(0, 0), m);
    assert!(view.demotions.is_empty());
}

#[test]
fn an_older_result_keeps_its_exact_form_as_the_history_grows() {
    let small = assemble(&policy(2_000, 1), history(6, 200));
    let large = assemble(&policy(2_000, 1), history(8, 200));
    assert_eq!(results(&small.messages)[0], results(&large.messages)[0]);
    let again = assemble(&policy(2_000, 1), history(6, 200));
    assert_eq!(
        serde_json::to_string(&small.messages).unwrap(),
        serde_json::to_string(&again.messages).unwrap()
    );
}

struct Always(Visibility);
impl VisibilityAdvisor for Always {
    fn advise(&self, _candidate: &Candidate<'_>, _default: Visibility) -> Visibility {
        self.0
    }
}

#[test]
fn an_advisor_may_choose_among_the_allowed_forms() {
    let view = policy(0, 0).assemble(
        history(3, 200),
        &HashSet::new(),
        &Always(Visibility::Hidden),
    );
    assert!(results(&view.messages)[0].contains("hidden"));
    assert!(view.demotions.iter().all(|d| d.to == Visibility::Hidden));
}

#[test]
fn an_advisor_can_never_drop_a_failure_or_a_pinned_result() {
    let mut messages = history(3, 200);
    let failing = format!("[exit 2]\n{}", log("f", 300));
    messages[2] = Message::tool_result("call0", &failing);
    let pinned_raw = results(&messages)[1].to_string();
    let pinned: HashSet<ChunkId> = [ChunkId::for_content(pinned_raw.as_bytes())].into();
    let view = policy(0, 0).assemble(messages, &pinned, &Always(Visibility::Hidden));
    assert!(results(&view.messages)[0].contains("| long |"));
    assert_eq!(results(&view.messages)[1], pinned_raw);
}

#[test]
fn an_advisor_cannot_promote_a_result_above_where_the_policy_left_it() {
    let view = policy(0, 0).assemble(history(3, 200), &HashSet::new(), &Always(Visibility::Full));
    // Full is not a demotion; the deterministic default applies instead.
    assert!(!view.demotions.is_empty());
    assert!(view.demotions.iter().all(|d| d.to < Visibility::Full));
}

// ── what a demoted result keeps because the conversation points at it ────────

fn source(lines: usize) -> String {
    (1..=lines)
        .map(|n| match n % 4 {
            1 => format!("L{n}:pub fn helper_{n}(x: u64) -> u64 {{"),
            2 => format!("L{n}:    x + {n} // body line {n}"),
            3 => format!("L{n}:}}"),
            _ => format!("L{n}:"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A read of `src/lib.rs`, a failing build that names line 98, then `rounds`
/// rounds of filler.
fn read_then_error(rounds: usize) -> Vec<Message> {
    let mut m = vec![Message::user("fix the build in `helper_217`")];
    m.push(call("r", "read_file", r#"{"path":"src/lib.rs"}"#));
    m.push(Message::tool_result("r", source(240)));
    m.push(Message::assistant("read it"));
    m.push(call("b", "shell", r#"{"command":"cargo build"}"#));
    m.push(Message::tool_result(
        "b",
        format!(
            "error[E0308]: mismatched types\n --> src/lib.rs:98:5\n{}",
            log("b", 20)
        ),
    ));
    m.push(Message::assistant("saw the error"));
    for r in 0..rounds {
        let id = format!("f{r}");
        m.push(call(&id, "shell", &format!(r#"{{"command":"step {r}"}}"#)));
        m.push(Message::tool_result(&id, log(&format!("f{r}"), 200)));
        m.push(Message::assistant(format!("done step {r}")));
    }
    m
}

#[test]
fn a_demoted_read_keeps_the_lines_that_later_output_names_and_the_definition_the_task_names() {
    let view = assemble(&policy(2_500, 1), read_then_error(6));
    let read = results(&view.messages)[0];
    assert!(read.contains("short") || read.contains("long"), "{read}");
    assert!(
        read.contains("body line 98"),
        "the line the error names: {read}"
    );
    assert!(
        read.contains("body line 218"),
        "the definition the task names: {read}"
    );
    assert!(read.contains("expand"), "{read}");
}

#[test]
fn what_a_result_keeps_does_not_change_when_later_output_names_more() {
    let before = assemble(&policy(2_500, 1), read_then_error(6));
    let mut later = read_then_error(6);
    later.push(Message::user("also look at src/lib.rs:200"));
    later.push(call("x", "shell", r#"{"command":"more"}"#));
    later.push(Message::tool_result("x", "error: see src/lib.rs:201"));
    later.push(Message::assistant("ok"));
    let after = assemble(&policy(2_500, 1), later);
    assert_eq!(
        results(&before.messages)[0],
        results(&after.messages)[0],
        "a form is fixed once the result leaves the recent rounds"
    );
}
