// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping agent context small
// without losing evidence for its clients. If your team needs expertise in
// agent context management then you can procure our services by sending an
// email to info@swedishembedded.com.
//! An expansion is a recovery event: the ledger counts it and the tokens it
//! brought back, so a bad demotion can be seen.

use sven_chunks::{render_expansion, ChunkId, ChunkKind, ExpansionRequest, Freshness};
use sven_model::{CompletionRequest, FunctionCall, Message, MessageContent, Role};
use sven_turn::ledger::estimate_request;
use sven_vocab::ContextRecovery;

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

fn request(messages: Vec<Message>) -> CompletionRequest {
    CompletionRequest {
        messages,
        ..CompletionRequest::default()
    }
}

fn expansion(freshness: &Freshness, text: &str) -> String {
    render_expansion(&ExpansionRequest {
        id: ChunkId::for_content(text.as_bytes()),
        kind: ChunkKind::File,
        total_lines: 4,
        range: None,
        freshness,
        text,
    })
}

#[test]
fn a_history_without_expansions_records_no_recovery() {
    let b = estimate_request(&request(vec![
        Message::user("hi"),
        call("1", "shell", r#"{"command":"ls"}"#),
        Message::tool_result("1", "a b"),
    ]));
    assert_eq!(b.context_recovery, ContextRecovery::default());
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("context_recovery"), "{json}");
}

#[test]
fn each_expansion_counts_with_the_bytes_and_tokens_it_brought_back() {
    let first = expansion(&Freshness::Fresh, &"x".repeat(400));
    let second = expansion(&Freshness::NotFileDerived, "short");
    let b = estimate_request(&request(vec![
        Message::user("go"),
        call(
            "1",
            "context",
            r#"{"action":"expand","chunk":"c-0123456789abcdef"}"#,
        ),
        Message::tool_result("1", &first),
        call(
            "2",
            "context",
            r#"{"action":"expand","chunk":"c-0123456789abcdef","first_line":1,"last_line":3}"#,
        ),
        Message::tool_result("2", &second),
        // Other actions of the same tool are not recovery events.
        call("3", "context", r#"{"action":"open","path":"a"}"#),
        Message::tool_result("3", "handle"),
    ]));
    let r = b.context_recovery;
    assert_eq!(r.expansions, 2);
    assert_eq!(r.expanded_bytes, (first.len() + second.len()) as u64);
    assert_eq!(
        r.expanded_tokens,
        (first.len().div_ceil(4) + second.len().div_ceil(4)) as u64
    );
    assert_eq!(r.stale_expansions, 0);
}

#[test]
fn an_expansion_of_a_stale_chunk_is_counted_as_stale() {
    let stale = Freshness::Stale {
        recorded: sven_chunks::SourceVersion {
            size: 1,
            mtime_ns: 1,
        },
        current: None,
    };
    let b = estimate_request(&request(vec![
        call(
            "1",
            "context",
            r#"{"action":"expand","chunk":"c-0123456789abcdef"}"#,
        ),
        Message::tool_result("1", expansion(&stale, "old")),
    ]));
    assert_eq!(
        (
            b.context_recovery.expansions,
            b.context_recovery.stale_expansions
        ),
        (1, 1)
    );
}

#[test]
fn an_expansion_whose_result_is_missing_still_counts_as_a_call() {
    let b = estimate_request(&request(vec![call(
        "1",
        "context",
        r#"{"action":"expand","chunk":"c-0123456789abcdef"}"#,
    )]));
    assert_eq!(b.context_recovery.expansions, 1);
    assert_eq!(b.context_recovery.expanded_bytes, 0);
}
