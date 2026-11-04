// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a recorded run can be reproduced from its trajectory.
//!
//! With a scripted model the whole run is a function of its scripts, its tools
//! and its sampling. The model's side of a run is rebuilt here from nothing but
//! the exported ATIF document, replayed through a fresh engine, and must give
//! the same events from the model and the same trajectory: the record holds
//! enough to reconstruct the stream it came from, exactly.
//!
//! Swedish Embedded AB implements reproducible agent runs for its clients. If
//! your team needs expertise in recording agent runs so they can be replayed
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use std::sync::Arc;

use serde_json::{json, Value};
use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::tool::{ApprovalPolicy, Tool, ToolCall, ToolCapability, ToolOutput};
use sven_sdk::{Engine, RunOptions};
use sven_vocab::{AdapterRef, ServedIdentity};

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
    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"city": {"type": "string"}}})
    }
    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        ToolOutput::ok(&call.id, "sunny")
    }
}

fn identity(seed: u64) -> ResponseEvent {
    ResponseEvent::Identity(ServedIdentity {
        seed: Some(seed),
        adapter: Some(AdapterRef {
            id: "tuned".into(),
            digest: "sha256:aa".into(),
        }),
        base_digest: Some("sha256:bb".into()),
        backend: Some("cpu".into()),
        engine: Some("brain 2.0.0".into()),
    })
}

fn usage(input: u32, output: u32) -> ResponseEvent {
    ResponseEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: None,
    }
}

/// A tool call, then the answer after its result: two requests, one step.
fn recorded_scripts() -> Vec<Vec<ResponseEvent>> {
    vec![
        vec![
            ResponseEvent::Served {
                model: "served-7b".into(),
                seed: Some(42),
            },
            ResponseEvent::Tokens {
                ids: vec![11, 12],
                logprobs: vec![-0.5, -0.25],
            },
            ResponseEvent::PromptTokens { ids: vec![1, 2, 3] },
            identity(4242),
            ResponseEvent::ToolCall {
                index: 0,
                id: "w1".into(),
                name: "weather".into(),
                arguments: r#"{"city":"Lund"}"#.into(),
            },
            usage(100, 2),
            ResponseEvent::Done,
        ],
        vec![
            ResponseEvent::Served {
                model: "served-7b".into(),
                seed: Some(42),
            },
            ResponseEvent::Tokens {
                ids: vec![13],
                logprobs: vec![-0.125],
            },
            ResponseEvent::PromptTokens {
                ids: vec![1, 2, 3, 11, 12, 77],
            },
            identity(4242),
            ResponseEvent::TextDelta("Sunny.".into()),
            usage(150, 1),
            ResponseEvent::Done,
        ],
    ]
}

/// The model's side of a run, rebuilt from its exported trajectory alone: one
/// script per model request. A step that covers several requests keeps each in
/// `extra.sven.requests`; its tool calls belong to the first, its text to the
/// last (a request that asks for tools and the one that answers with the
/// result).
fn scripts_from(trajectory: &atif::Trajectory) -> Vec<Vec<ResponseEvent>> {
    let mut scripts = Vec::new();
    for step in trajectory.steps.iter().filter(|s| s.metrics.is_some()) {
        let metrics = step.metrics.as_ref().unwrap();
        let sven = &metrics.extra.as_ref().expect("sven extra")["sven"];
        let model = step.model_name.clone().expect("model");
        let requests: Vec<Value> = sven["requests"].as_array().cloned().unwrap_or_else(|| {
            vec![json!({
                "seed": sven["seed"],
                "prompt_tokens": metrics.prompt_tokens,
                "completion_tokens": metrics.completion_tokens,
                "prompt_token_ids": metrics.prompt_token_ids,
                "completion_token_ids": metrics.completion_token_ids,
                "logprobs": metrics.logprobs,
            })]
        });
        let last = requests.len() - 1;
        for (i, request) in requests.iter().enumerate() {
            let ids = |key: &str| -> Vec<i64> {
                serde_json::from_value(request[key].clone()).unwrap_or_default()
            };
            let mut events = vec![ResponseEvent::Served {
                model: model.clone(),
                // The request carried the sampling seed; the server's own seed
                // is in the identity event.
                seed: Some(42),
            }];
            events.push(ResponseEvent::Tokens {
                ids: ids("completion_token_ids"),
                logprobs: serde_json::from_value(request["logprobs"].clone()).unwrap_or_default(),
            });
            events.push(ResponseEvent::PromptTokens {
                ids: ids("prompt_token_ids"),
            });
            events.push(ResponseEvent::Identity(ServedIdentity {
                seed: request["seed"].as_u64(),
                adapter: Some(AdapterRef {
                    id: sven["adapter"]["id"].as_str().unwrap().into(),
                    digest: sven["adapter"]["digest"].as_str().unwrap().into(),
                }),
                base_digest: sven["base_digest"].as_str().map(str::to_string),
                backend: sven["backend"].as_str().map(str::to_string),
                engine: sven["engine"].as_str().map(str::to_string),
            }));
            if i == 0 {
                for call in step.tool_calls.iter().flatten() {
                    events.push(ResponseEvent::ToolCall {
                        index: 0,
                        id: call.tool_call_id.clone(),
                        name: call.function_name.clone(),
                        arguments: call.arguments.to_string(),
                    });
                }
            }
            if i == last {
                let text = serde_json::to_value(&step.message)
                    .unwrap()
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                // A response that only called a tool said nothing.
                if !text.is_empty() {
                    events.push(ResponseEvent::TextDelta(text));
                }
            }
            let prompt = request["prompt_tokens"].as_u64().unwrap_or(0) as u32;
            let completion = request["completion_tokens"].as_u64().unwrap_or(0) as u32;
            events.push(usage(prompt, completion));
            events.push(ResponseEvent::Done);
            scripts.push(events);
        }
    }
    scripts
}

/// Drops the fields that measure when or for how long something happened, not
/// what: a timestamp and a tool call's wall-clock duration.
fn without_timestamps(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            map.remove("timestamp");
            map.remove("duration_ms");
            for v in map.values_mut() {
                *v = without_timestamps(v.take());
            }
        }
        Value::Array(items) => {
            for v in items {
                *v = without_timestamps(v.take());
            }
        }
        _ => {}
    }
    value
}

async fn run(scripts: Vec<Vec<ResponseEvent>>) -> atif::Trajectory {
    let engine = Engine::builder()
        .model_provider(Arc::new(ScriptedMockProvider::new(scripts)) as Arc<_>)
        .tool(Arc::new(Weather))
        .build()
        .expect("an engine builds");
    let mut agent = engine.agent("agent");
    agent
        .send_with(
            "weather in Lund?",
            RunOptions::new()
                .sampling(sven_model::SamplingParams {
                    seed: Some(42),
                    ..Default::default()
                })
                .want_logprobs(true),
        )
        .await
        .expect("an outcome");
    agent.trajectory()
}

#[tokio::test]
async fn the_models_side_of_a_run_is_rebuilt_exactly_from_its_trajectory() {
    let original = recorded_scripts();
    let recorded = run(original.clone()).await;

    let rebuilt = scripts_from(&recorded);
    assert_eq!(
        format!("{rebuilt:?}"),
        format!("{original:?}"),
        "the trajectory alone gives back the model's event stream"
    );
}

#[tokio::test]
async fn replaying_the_rebuilt_stream_gives_the_same_trajectory() {
    let recorded = run(recorded_scripts()).await;
    let replayed = run(scripts_from(&recorded)).await;
    assert_eq!(
        without_timestamps(serde_json::to_value(&replayed).unwrap()),
        without_timestamps(serde_json::to_value(&recorded).unwrap())
    );
    // And a second run of the same scripts is the same again: nothing in the
    // record depends on when or how often it was made.
    let again = run(recorded_scripts()).await;
    assert_eq!(
        without_timestamps(serde_json::to_value(&again).unwrap()),
        without_timestamps(serde_json::to_value(&recorded).unwrap())
    );
}
