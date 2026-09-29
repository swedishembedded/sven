// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a run is bounded, and says how it ended.
//!
//! An embedded agent must be stoppable from outside (a cancel token), must
//! not outlive its deadline, must stop spending once its output-token budget
//! is gone, and must report which of those ended it. The reply alone cannot
//! say whether the agent finished, ran out of rounds or was cut off.
//!
//! Swedish Embedded AB implements bounded, observable agent runtimes for its
//! clients. If your team needs expertise in agent execution control then you
//! can procure our services by sending an email to info@swedishembedded.com.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use sven_model::{CompletionRequest, ModelProvider, ResponseEvent, ResponseStream};
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::config::Config;
use sven_sdk::{CancelToken, Engine, RunConclusion, RunOptions, Toolset};

fn says(text: &str) -> Vec<ResponseEvent> {
    vec![ResponseEvent::TextDelta(text.into()), ResponseEvent::Done]
}

/// A model that starts answering and never finishes.
struct Stalls;

#[async_trait::async_trait]
impl ModelProvider for Stalls {
    fn name(&self) -> &str {
        "stalls"
    }
    fn model_name(&self) -> &str {
        "stalls"
    }
    async fn complete(&self, _req: CompletionRequest) -> anyhow::Result<ResponseStream> {
        let first = futures::stream::iter([Ok(ResponseEvent::TextDelta("thinking".into()))]);
        Ok(first.chain(futures::stream::pending()).boxed())
    }
}

fn engine(provider: Arc<dyn ModelProvider>) -> Engine {
    Engine::builder()
        .model_provider(provider)
        .build()
        .expect("an engine builds")
}

#[tokio::test]
async fn a_finished_run_reports_success_and_its_usage() {
    let script = vec![
        ResponseEvent::TextDelta("hello".into()),
        ResponseEvent::Usage {
            input_tokens: 12,
            output_tokens: 3,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd: None,
        },
        ResponseEvent::Done,
    ];
    let mut agent = engine(Arc::new(ScriptedMockProvider::new(vec![script]))).agent("agent");
    let outcome = agent.send("hi").await.expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Success);
    assert_eq!(outcome.reply, "hello");
    assert_eq!(outcome.usage.output_tokens, Some(3));
}

#[tokio::test]
async fn an_unmeasured_usage_is_none() {
    let mut agent = engine(Arc::new(ScriptedMockProvider::new(vec![says("hi")]))).agent("agent");
    let outcome = agent.send("hi").await.expect("an outcome");
    assert_eq!(outcome.usage.output_tokens, None);
}

#[tokio::test]
async fn a_cancelled_run_stops_and_says_so() {
    let mut agent = engine(Arc::new(Stalls)).agent("agent");
    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.cancel();
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        agent.send_with("hi", RunOptions::new().cancel(cancel)),
    )
    .await
    .expect("cancel stops the run")
    .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Cancelled);
}

#[tokio::test]
async fn a_run_past_its_deadline_times_out() {
    let mut agent = engine(Arc::new(Stalls)).agent("agent");
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        agent.send_with("hi", RunOptions::new().deadline(Duration::from_millis(150))),
    )
    .await
    .expect("the deadline stops the run")
    .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::Timeout);
}

#[tokio::test]
async fn a_spent_output_budget_ends_the_run() {
    let tool_round = |id: &str| {
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: id.into(),
                name: "read_file".into(),
                arguments: r#"{"path":"Cargo.toml"}"#.into(),
            },
            ResponseEvent::Usage {
                input_tokens: 10,
                output_tokens: 600,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd: None,
            },
            ResponseEvent::Done,
        ]
    };
    let provider = ScriptedMockProvider::new(vec![tool_round("a"), tool_round("b"), says("done")]);
    let engine = Engine::builder()
        .model_provider(Arc::new(provider))
        .toolset(Toolset::research())
        .build()
        .expect("an engine builds");
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        engine
            .agent("agent")
            .send_with("read it", RunOptions::new().max_output_tokens(500)),
    )
    .await
    .expect("the budget stops the run")
    .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::BudgetExhausted);
    assert_eq!(outcome.usage.output_tokens, Some(600));
}

#[tokio::test]
async fn running_out_of_tool_rounds_is_not_reported_as_success() {
    let tool_round = |id: &str| {
        vec![
            ResponseEvent::ToolCall {
                index: 0,
                id: id.into(),
                name: "read_file".into(),
                arguments: r#"{"path":"Cargo.toml"}"#.into(),
            },
            ResponseEvent::Done,
        ]
    };
    let provider = ScriptedMockProvider::new(vec![
        tool_round("a"),
        tool_round("b"),
        tool_round("c"),
        says("wrapped up"),
    ]);
    let mut config = Config::default();
    config.agent.max_tool_rounds = 1;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::new(provider))
        .toolset(Toolset::research())
        .build()
        .expect("an engine builds");
    let outcome = tokio::time::timeout(Duration::from_secs(20), engine.agent("agent").send("go"))
        .await
        .expect("the run ends")
        .expect("an outcome");
    assert_eq!(outcome.conclusion, RunConclusion::BudgetExhausted);
}
