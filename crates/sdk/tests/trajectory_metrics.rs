// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a run reports what it cost and the trajectory records it.
//!
//! The outcome of a run carries tokens (cached ones too), cost, tool calls,
//! tool errors, turns and wall time. The trajectory carries the same account
//! per step and in total, and names the model and seed that served each
//! request, so a run can be priced, compared and reproduced later.
//!
//! Swedish Embedded AB implements agent cost accounting for its clients. If
//! your team needs expertise in measuring what agent runs cost then you can
//! procure our services by sending an email to info@swedishembedded.com.

mod common;

use std::sync::Arc;

use common::Strict;
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::{
    tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput},
    Engine,
};

struct Weather;

#[async_trait::async_trait]
impl Tool for Weather {
    fn name(&self) -> &str {
        "weather"
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ReadFile
    }
    fn description(&self) -> &str {
        "Today's weather in a city."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "sunny")
    }
}

fn usage(input: u32, output: u32, cache_read: u32, cost: f64) -> ResponseEvent {
    ResponseEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
        cost_usd: Some(cost),
    }
}

/// Two requests: a tool call, then the answer.
async fn two_request_agent() -> (Engine, sven_sdk::Agent, sven_sdk::RunOutcome) {
    let scripts = vec![
        vec![
            ResponseEvent::Served {
                model: "served-7b".into(),
                seed: Some(42),
            },
            ResponseEvent::Tokens {
                ids: vec![11, 12],
                logprobs: vec![-0.5, -0.25],
            },
            ResponseEvent::ToolCall {
                index: 0,
                id: "w1".into(),
                name: "weather".into(),
                arguments: r#"{"city":"Lund"}"#.into(),
            },
            usage(100, 10, 40, 0.5),
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::TextDelta("Sunny.".into()),
            usage(150, 5, 60, 0.25),
            ResponseEvent::Done,
        ],
    ];
    let engine = Engine::builder()
        .model_provider(Arc::new(Strict(ScriptedMockProvider::new(scripts))))
        .tool(Arc::new(Weather))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    let outcome = agent.send("weather in Lund?").await.expect("an outcome");
    (engine, agent, outcome)
}

#[tokio::test]
async fn the_outcome_reports_tokens_cost_tools_turns_and_time() {
    let (_, _agent, outcome) = two_request_agent().await;
    let usage = outcome.usage;
    assert_eq!(usage.input_tokens, Some(250));
    assert_eq!(usage.output_tokens, Some(15));
    assert_eq!(usage.cached_tokens, Some(100));
    assert_eq!(usage.cost_usd, Some(0.75));
    assert_eq!(usage.tool_calls, 1);
    assert_eq!(usage.tool_errors, 0);
    assert_eq!(usage.turns, 2);
    assert!(usage.wall_time > std::time::Duration::ZERO);
}

#[tokio::test]
async fn trajectory_totals_equal_the_sum_of_its_steps() {
    let (_, agent, _) = two_request_agent().await;
    let trajectory = agent.trajectory();
    sven_sdk::atif::validate_trajectory(&trajectory).expect("a valid ATIF document");

    let sum = |field: fn(&sven_sdk::atif::StepMetrics) -> Option<u64>| -> u64 {
        trajectory
            .steps
            .iter()
            .filter_map(|s| s.metrics.as_ref())
            .filter_map(field)
            .sum()
    };
    let total = trajectory.final_metrics.as_ref().expect("final metrics");
    // Prompt tokens count everything the model read: uncached plus cached.
    assert_eq!(total.total_prompt_tokens, Some(350));
    assert_eq!(total.total_completion_tokens, Some(15));
    assert_eq!(total.total_cached_tokens, Some(100));
    assert_eq!(total.total_cost_usd, Some(0.75));
    assert_eq!(total.total_prompt_tokens, Some(sum(|m| m.prompt_tokens)));
    assert_eq!(
        total.total_completion_tokens,
        Some(sum(|m| m.completion_tokens))
    );
    assert_eq!(total.total_cached_tokens, Some(sum(|m| m.cached_tokens)));
    let step_cost: f64 = trajectory
        .steps
        .iter()
        .filter_map(|s| s.metrics.as_ref()?.cost_usd)
        .sum();
    assert!((step_cost - 0.75).abs() < 1e-9);
}

#[tokio::test]
async fn the_served_model_seed_and_sampled_tokens_are_on_the_step() {
    let (_, agent, _) = two_request_agent().await;
    let trajectory = agent.trajectory();
    let step = trajectory
        .steps
        .iter()
        .find(|s| s.tool_calls.is_some())
        .expect("the step that called the tool");
    assert_eq!(step.model_name.as_deref(), Some("served-7b"));
    let metrics = step.metrics.as_ref().expect("step metrics");
    assert_eq!(metrics.completion_token_ids, Some(vec![11, 12]));
    assert_eq!(metrics.logprobs, Some(vec![-0.5, -0.25]));
    assert_eq!(metrics.extra.as_ref().unwrap()["sven"]["seed"], 42);
}

#[tokio::test]
async fn a_resumed_agent_exports_the_same_account() {
    let (engine, agent, _) = two_request_agent().await;
    let state = serde_json::to_string(agent.state()).expect("the state serializes");
    let resumed = engine
        .resume(serde_json::from_str(&state).expect("the state reads"))
        .expect("it resumes");
    assert_eq!(resumed.trajectory().steps, agent.trajectory().steps);
    assert_eq!(
        resumed.trajectory().final_metrics,
        agent.trajectory().final_metrics
    );
}
