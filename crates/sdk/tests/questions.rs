// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a question the model asks never makes a run wait for a person.
//!
//! With nobody to ask it is answered at once, saying so, and the run carries
//! on. A host that chose to park questions gets the run back at once as
//! `Waiting` with the question - the answer may take minutes or days - and
//! `answer` continues it from where it stopped, also after a suspend and
//! resume in another process, with the answer as the result of the call that
//! asked. A host with a person answers it on the spot.
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
    builder(scripts)
        .park_questions()
        .build()
        .expect("an engine builds")
}

fn builder(scripts: Vec<Vec<ResponseEvent>>) -> sven_sdk::EngineBuilder {
    Engine::builder()
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .toolset(Toolset::research())
}

/// The result the call `q1` got in the agent's history.
fn answer_to_q1(agent: &sven_sdk::Agent) -> String {
    agent
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
        .expect("the question has a result")
}

#[tokio::test]
async fn with_nobody_to_ask_a_question_is_answered_at_once_and_the_run_goes_on() {
    let engine = builder(vec![asks(), concludes()])
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        agent.send("start a web service"),
    )
    .await
    .expect("the run never waits for a person")
    .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert!(outcome.question.is_none());
    let answer = answer_to_q1(&agent);
    assert!(answer.contains("No user is available"), "{answer}");
}

#[tokio::test]
async fn a_host_with_a_person_answers_the_question_on_the_spot() {
    let engine = builder(vec![asks(), concludes()])
        .human_gates(|gate| {
            if let sven_sdk::HumanGate::Question { prompt, reply_tx } = gate {
                assert!(prompt.contains("Which framework?") && prompt.contains("Axum"));
                let _ = reply_tx.send("Axum".into());
            }
        })
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = agent.send("start a web service").await.expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert!(answer_to_q1(&agent).contains("Axum"));
}

#[tokio::test]
async fn a_parked_question_ends_the_run_and_its_answer_resumes_it() {
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

/// A handler that drops a question without replying does not leave the run
/// waiting: the machine gets the no-user answer and goes on.
#[tokio::test]
async fn a_question_the_handler_ignores_gets_the_no_user_answer() {
    let decision =
        |d: serde_json::Value| vec![ResponseEvent::TextDelta(d.to_string()), ResponseEvent::Done];
    let model = ScriptedMockProvider::new(vec![
        decision(serde_json::json!({"status": "need_user_input", "message": "Which database?"})),
        decision(serde_json::json!({"status": "failed", "message": "stopping"})),
    ]);
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let model = Recording(model, Arc::clone(&requests));
    let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&asked);
    let engine = Engine::builder()
        .model_provider(Arc::new(model))
        .human_gates(move |gate| {
            if let sven_sdk::HumanGate::Question { .. } = gate {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        })
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("sdlc");
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        agent.send("add a user store"),
    )
    .await
    .expect("the run never waits for the dropped question")
    .ok();
    assert!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        "the handler was asked"
    );
    let requests = requests.lock().unwrap();
    assert!(
        requests.iter().any(|r| r.contains("No user is available")),
        "the machine got the no-user answer: {requests:?}"
    );
}

/// Records every request it is sent, then delegates to the script.
struct Recording(ScriptedMockProvider, Arc<std::sync::Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl sven_model::ModelProvider for Recording {
    fn name(&self) -> &str {
        "recording"
    }
    fn model_name(&self) -> &str {
        "recording"
    }
    async fn complete(
        &self,
        req: sven_model::CompletionRequest,
    ) -> anyhow::Result<sven_model::ResponseStream> {
        self.1.lock().unwrap().push(format!("{req:?}"));
        self.0.complete(req).await
    }
}
