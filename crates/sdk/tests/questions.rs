// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a question the model asks parks the run until someone answers it.
//!
//! The answer may take minutes or days, so the run does not block on it: it
//! stops as `Waiting` with the question, the agent can be suspended and
//! resumed by another process, and `answer` continues it from where it
//! stopped, with the answer as the result of the call that asked.
//!
//! Swedish Embedded AB implements human-in-the-loop agent runtimes for its
//! clients. If your team needs expertise in long-running agent workflows then
//! you can procure our services by sending an email to
//! info@swedishembedded.com.

mod common;

use std::sync::Arc;

use common::Strict;
use sven_model::{MessageContent, ResponseEvent};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{AgentState, Engine, RunConclusion, Toolset};

/// The model asks which framework to use.
fn asks() -> Vec<ResponseEvent> {
    let ask = serde_json::json!({
        "questions": [{"prompt": "Which framework?", "options": ["Axum", "Actix"]}]
    });
    vec![
        ResponseEvent::ToolCall {
            index: 0,
            id: "q1".into(),
            name: "ask_question".into(),
            arguments: ask.to_string(),
        },
        ResponseEvent::Done,
    ]
}

/// The model answers, having read the human's choice.
fn concludes() -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta("Using Axum.".into()),
        ResponseEvent::Done,
    ]
}

fn engine(scripts: Vec<Vec<ResponseEvent>>) -> Engine {
    Engine::builder()
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .toolset(Toolset::research())
        .build()
        .expect("an engine builds")
}

#[tokio::test]
async fn a_question_parks_the_run_and_its_answer_resumes_it() {
    let mut agent = engine(vec![asks()]).agent("agent");
    let outcome = agent.send("start a web service").await.expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Waiting);
    let question = outcome.question.expect("the run reports what it waits for");
    assert_eq!(question.prompt, "Which framework?");
    assert_eq!(question.options, vec!["Axum", "Actix"]);

    // Another process picks it up later.
    let saved = serde_json::to_string(&agent.suspend()).expect("the state serializes");
    let state: AgentState = serde_json::from_str(&saved).expect("and reads back");
    let mut agent = engine(vec![concludes()])
        .resume(state)
        .expect("the agent resumes");

    let outcome = agent
        .answer(&question.id, "Axum")
        .await
        .expect("the answered run continues");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert_eq!(outcome.reply, "Using Axum.");
    assert!(outcome.question.is_none());
    let result = agent
        .state()
        .history()
        .iter()
        .find_map(|m| match &m.content {
            MessageContent::ToolResult {
                tool_call_id,
                content,
            } if tool_call_id == "q1" => Some(format!("{content:?}")),
            _ => None,
        })
        .expect("the answer is the result of the call that asked");
    assert!(result.contains("Axum"), "{result}");
}

#[tokio::test]
async fn an_answer_to_another_question_is_refused() {
    let mut agent = engine(vec![asks()]).agent("agent");
    agent.send("start a web service").await.expect("an outcome");
    let err = agent
        .answer("00000000-0000-0000-0000-000000000000", "Axum")
        .await
        .expect_err("nothing asked that");
    assert!(matches!(err, sven_sdk::CallError::Precondition(_)), "{err}");
}
