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
    assert!(turn.unparsed_arguments.contains_key(&broken.id));
    assert!(
        !turn.unparsed_arguments.contains_key(&good.id),
        "a well-formed call must not be marked unparsed: {:?}",
        turn.unparsed_arguments
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

/// The text a call was streamed as is kept only where it is the JSON the call
/// runs on: a repaired call, a garbled one and an inline one have none, so
/// history never repeats text the tool did not get.
#[tokio::test]
async fn generated_argument_text_is_kept_only_for_calls_that_run_on_it() {
    let turn = run(vec![
        tool_call(0, "call_a", "grep", "{\"pattern\" :"),
        tool_call(0, "", "", " \"a\",  \"path\": \"x\"}"),
        tool_call(1, "call_b", "read_file", r#"{"path":"\c"}"#),
        tool_call(2, "call_c", "edit_file", "<<not json>>"),
        ResponseEvent::Done,
    ])
    .await;

    assert_eq!(turn.tool_calls.len(), 3);
    assert_eq!(
        turn.generated_arguments.get("call_a").map(String::as_str),
        Some("{\"pattern\" : \"a\",  \"path\": \"x\"}")
    );
    assert!(!turn.generated_arguments.contains_key("call_b"), "repaired");
    assert!(!turn.generated_arguments.contains_key("call_c"), "garbled");
}
