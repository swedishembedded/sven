// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent runtimes whose recorded sessions can be
// resumed and trained on as they ran, for its clients. If your team needs
// expertise in agent trajectories then you can procure our services by
// sending an email to info@swedishembedded.com.

//! Spec: a session resumed from its trajectory sends the history it sent
//! before, byte for byte: each reply's text, its reasoning, each call's
//! argument text as written (key order, spacing, text that is not JSON) and
//! the server's `sampled` record, then the results and the person's turns.

use serde_json::value::RawValue;
use sven_model::{reply_messages, Message};
use sven_session_store::{steps_to_messages, StepAssembler};
use sven_vocab::{ReplyFinish, SampledCall, SampledReply};

fn reply(text: &str, reasoning: &str, calls: &[(&str, &str)], sampled: &str) -> SampledReply {
    SampledReply {
        text: text.into(),
        reasoning: reasoning.into(),
        calls: calls
            .iter()
            .map(|(id, arguments)| SampledCall {
                id: (*id).into(),
                name: "shell".into(),
                arguments: (*arguments).into(),
            })
            .collect(),
        sampled: Some(RawValue::from_string(sampled.into()).unwrap()),
        finish: ReplyFinish::Stop,
    }
}

#[test]
fn a_session_resumed_from_its_trajectory_sends_the_same_history() {
    let mut history = vec![Message::user("fix it")];
    history.extend(reply_messages(&reply(
        "Looking first. ",
        "\n check the files \n",
        &[
            ("c1", r#"{ "z": 1, "cmd" :"ls" }"#),
            ("c2", r#"{"cmd": "\{"}"#),
        ],
        r#"{"token_ids":"AQID", "context_len":12}"#,
    )));
    history.push(Message::tool_result("c1", "a.txt"));
    history.push(Message::tool_result("c2", "the arguments cannot be read"));
    history.extend(reply_messages(&reply(
        "",
        "",
        &[("c3", r#"{"cmd":"cat a.txt"}"#)],
        r#"{"token_ids":"BA=="}"#,
    )));
    history.push(Message::tool_result("c3", "hello"));
    history.push(Message::user("and now?"));

    let mut assembler = StepAssembler::new();
    for message in &history {
        assembler.push_message(message);
    }
    let steps = assembler.finish();
    let stored = serde_json::to_string(&steps).unwrap();
    let reread: Vec<atif::TraceStep> = serde_json::from_str(&stored).unwrap();
    let resumed = steps_to_messages(&reread);

    assert_eq!(
        serde_json::to_string(&resumed).unwrap(),
        serde_json::to_string(&history).unwrap()
    );
}
