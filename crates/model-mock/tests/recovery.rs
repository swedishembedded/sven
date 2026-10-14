// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A scripted model that follows a reference back to the raw text: the rule
//! it follows is fixed and knows nothing of which lines matter.

use futures::StreamExt;
use sven_model::{
    CompletionRequest, FunctionCall, Message, MessageContent, ModelProvider, ResponseEvent, Role,
    ToolSchema,
};
use sven_model_mock::YamlMockProvider;

const RULE: &str = r#"
responses:
  - match_type: contains
    pattern: "root cause"
    reply: "Answered from the evidence in context."
    recover:
      reference: 'context \{"action":"expand","chunk":"(c-[0-9a-f]{16})"\}'
      call:
        tool: context
        args: {action: expand, chunk: "{1}"}
      focus: ["settle_ledger_ab12"]
"#;

fn demoted(id: &str, body: &str) -> String {
    format!(
        "[chunk {id} | short | file | 40 lines]\n{body}\n[omitted 30 lines; expand: context {{\"action\":\"expand\",\"chunk\":\"{id}\"}}, optional first_line/last_line]"
    )
}

fn offered(names: &[&str]) -> Vec<ToolSchema> {
    names
        .iter()
        .map(|n| ToolSchema {
            name: (*n).into(),
            description: String::new(),
            parameters: serde_json::json!({}),
            ..Default::default()
        })
        .collect()
}

fn call(id: &str, name: &str, arguments: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        },
    }
}

fn request(history: Vec<Message>, tools: &[&str]) -> CompletionRequest {
    let mut messages = history;
    messages.push(Message::user("What is the root cause?"));
    CompletionRequest {
        messages,
        tools: offered(tools),
        stream: true,
        ..Default::default()
    }
}

async fn events(p: &YamlMockProvider, req: CompletionRequest) -> Vec<ResponseEvent> {
    let mut out = Vec::new();
    let mut stream = p.complete(req).await.unwrap();
    while let Some(e) = stream.next().await {
        out.push(e.unwrap());
    }
    out
}

fn expanded(events: &[ResponseEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            ResponseEvent::ToolCall {
                name, arguments, ..
            } if name == "context" => {
                let args: serde_json::Value = serde_json::from_str(arguments).unwrap();
                assert_eq!(args["action"], "expand");
                Some(args["chunk"].as_str().unwrap().to_string())
            }
            _ => None,
        })
        .collect()
}

fn replied(events: &[ResponseEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e, ResponseEvent::TextDelta(t) if t.contains("Answered")))
}

fn history(results: &[(&str, String)]) -> Vec<Message> {
    let mut messages = Vec::new();
    for (n, (id, text)) in results.iter().enumerate() {
        let call_id = format!("t-{n}");
        messages.push(Message::user(format!("turn {n}")));
        messages.push(call(
            &call_id,
            "read_file",
            &format!("{{\"path\":\"{id}\"}}"),
        ));
        messages.push(Message::tool_result(&call_id, text));
    }
    messages
}

#[tokio::test]
async fn a_referenced_chunk_that_mentions_the_focus_is_expanded() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let past = history(&[
        (
            "a",
            demoted(
                "c-00000000000000aa",
                "outline: pub fn settle_ledger_ab12(x)",
            ),
        ),
        (
            "b",
            demoted("c-00000000000000bb", "outline: pub fn unrelated(x)"),
        ),
    ]);
    let e = events(&p, request(past, &["context"])).await;
    assert_eq!(expanded(&e), ["c-00000000000000aa"]);
    assert!(!replied(&e), "the reply waits for the raw text");
}

#[tokio::test]
async fn nothing_is_expanded_when_no_form_mentions_the_focus() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let past = history(&[(
        "b",
        demoted("c-00000000000000bb", "outline: pub fn unrelated(x)"),
    )]);
    let e = events(&p, request(past, &["context"])).await;
    assert!(expanded(&e).is_empty());
    assert!(replied(&e));
}

#[tokio::test]
async fn a_whole_result_is_not_a_reference() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let past = history(&[("a", "pub fn settle_ledger_ab12(x) { 1 }".into())]);
    let e = events(&p, request(past, &["context"])).await;
    assert!(expanded(&e).is_empty());
    assert!(replied(&e));
}

#[tokio::test]
async fn a_model_cannot_call_a_tool_it_was_not_offered() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let past = history(&[(
        "a",
        demoted("c-00000000000000aa", "pub fn settle_ledger_ab12(x)"),
    )]);
    let e = events(&p, request(past, &["read_file"])).await;
    assert!(expanded(&e).is_empty());
    assert!(replied(&e));
}

#[tokio::test]
async fn once_the_raw_text_is_back_the_model_answers() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let mut past = history(&[(
        "a",
        demoted("c-00000000000000aa", "pub fn settle_ledger_ab12(x)"),
    )]);
    past.push(Message::user("What is the root cause?"));
    past.push(call(
        "mock-recover-0",
        "context",
        r#"{"action":"expand","chunk":"c-00000000000000aa"}"#,
    ));
    past.push(Message::tool_result("mock-recover-0", "raw text"));
    let req = CompletionRequest {
        messages: past,
        tools: offered(&["context"]),
        stream: true,
        ..Default::default()
    };
    let e = events(&p, req).await;
    assert!(expanded(&e).is_empty());
    assert!(replied(&e));
}

#[tokio::test]
async fn a_chunk_already_expanded_earlier_is_not_expanded_again() {
    let p = YamlMockProvider::load(RULE).unwrap();
    let mut past = history(&[(
        "a",
        demoted("c-00000000000000aa", "pub fn settle_ledger_ab12(x)"),
    )]);
    past.push(Message::user("Earlier question."));
    past.push(call(
        "e-1",
        "context",
        r#"{"action":"expand","chunk":"c-00000000000000aa"}"#,
    ));
    past.push(Message::tool_result("e-1", "raw text"));
    let e = events(&p, request(past, &["context"])).await;
    assert!(expanded(&e).is_empty());
    assert!(replied(&e));
}
