// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for reliable tool execution by LLM
// agents for its clients. If your team needs expertise in agent tool-call
// handling then you can procure our services by sending an email to
// info@swedishembedded.com.
//! Spec: every tool call of a turn has an id of its own, whatever ids the
//! server sent, and a call whose arguments are garbled is reported as
//! unparsed under its own id and under no other call's.
//! The argument text a call was streamed as is kept beside it only where
//! that text is the JSON the call runs on.

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_turn::{stream_turn, TurnLimits};
use tokio::sync::mpsc;

fn tool_call(index: u32, id: &str, name: &str, arguments: &str) -> ResponseEvent {
    ResponseEvent::ToolCall {
        index,
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

async fn run(script: Vec<ResponseEvent>) -> sven_turn::StreamedTurn {
    let provider = ScriptedMockProvider::new(vec![script]);
    let (tx, mut rx) = mpsc::channel(256);
    let turn = stream_turn(
        &provider,
        vec![],
        vec![],
        None,
        None,
        None,
        TurnLimits::default(),
        &tx,
    )
    .await
    .expect("the turn completes");
    drop(tx);
    while rx.recv().await.is_some() {}
    turn
}

#[tokio::test]
async fn parallel_calls_sent_with_one_id_get_distinct_ids() {
    let turn = run(vec![
        tool_call(0, "call_0", "grep", r#"{"pattern":"a"}"#),
        tool_call(1, "call_0", "read_file", r#"{"path":"x"}"#),
        tool_call(2, "call_0", "find_file", r#"{"pattern":"*"}"#),
        ResponseEvent::Done,
    ])
    .await;

    let ids: Vec<&str> = turn.tool_calls.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), 3, "{ids:?}");
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 3, "call ids must be unique: {ids:?}");
    assert_eq!(ids[0], "call_0", "the first keeps the server's id");
}

#[tokio::test]
async fn an_unparsed_call_is_reported_under_its_own_id_only() {
    let turn = run(vec![
        tool_call(0, "call_0", "grep", r#"{"pattern":"a"}"#),
        tool_call(1, "call_0", "edit_file", "<<not json>>"),
        ResponseEvent::Done,
    ])
    .await;

    let broken = turn
        .tool_calls
        .iter()
        .find(|c| c.name == "edit_file")
        .expect("the garbled call is still proposed");
    let good = turn.tool_calls.iter().find(|c| c.name == "grep").unwrap();
    assert!(turn.arg_errors.contains_key(&broken.id));
    assert!(
        !turn.arg_errors.contains_key(&good.id),
        "a well-formed call must not be marked unparsed: {:?}",
        turn.arg_errors
    );
    assert_eq!(good.args["pattern"], "a");
}

#[tokio::test]
async fn a_reply_cut_at_the_output_limit_says_so() {
    let cut = run(vec![
        ResponseEvent::TextDelta("half an ans".into()),
        ResponseEvent::MaxTokens,
        ResponseEvent::Done,
    ])
    .await;
    assert!(cut.truncated);
    let whole = run(vec![
        ResponseEvent::TextDelta("done".into()),
        ResponseEvent::Done,
    ])
    .await;
    assert!(!whole.truncated);
}

/// Every call keeps the text the model streamed, whether or not it parses,
/// in the order the model wrote the calls; a call whose text is not a JSON
/// object runs no tool and says why, with nothing repaired.
#[tokio::test]
async fn every_call_keeps_its_text_and_one_that_cannot_run_says_why() {
    let turn = run(vec![
        tool_call(2, "call_c", "edit_file", "<<not json>>"),
        tool_call(0, "call_a", "grep", "{\"pattern\" :"),
        tool_call(1, "call_b", "read_file", r#"{"path":"\c"}"#),
        tool_call(0, "", "", " \"a\",  \"path\": \"x\"} "),
        tool_call(3, "call_d", "ls", "[1]"),
        ResponseEvent::Done,
    ])
    .await;

    let texts: Vec<(&str, &str)> = turn
        .reply
        .calls
        .iter()
        .map(|c| (c.id.as_str(), c.arguments.as_str()))
        .collect();
    assert_eq!(
        texts,
        vec![
            ("call_a", "{\"pattern\" : \"a\",  \"path\": \"x\"} "),
            ("call_b", r#"{"path":"\c"}"#),
            ("call_c", "<<not json>>"),
            ("call_d", "[1]"),
        ]
    );
    assert_eq!(turn.tool_calls[0].args["path"], "x");
    for (id, want) in [
        ("call_b", "not valid JSON"),
        ("call_c", "not valid JSON"),
        ("call_d", "not an object"),
    ] {
        let why = turn
            .arg_errors
            .get(id)
            .map(ToString::to_string)
            .unwrap_or_default();
        assert!(why.contains(want), "{id}: {why}");
    }
    assert!(!turn.arg_errors.contains_key("call_a"));
}
