// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.

//! Spec: an exported run has one agent step per model response. The calls of
//! one response, with their results, are one step; the next response opens a
//! new step, so a consumer that trains on turns can tell where each model
//! answer began and ended, and each request's tokens are on the step that
//! answered it.

use atif::{StepOrigin, TraceStep};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_session_store::record_steps::{conversation_steps, ConversationFacts};
use sven_vocab::{ReportedUsage, RequestRecord};

fn call(id: &str, name: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: MessageContent::ToolCall {
            tool_call_id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: "{}".into(),
            },
        },
        reasoning: None,
    }
}

fn result(id: &str, text: &str) -> Message {
    Message::tool_result(id, text)
}

fn request(input: u32, output: u32) -> RequestRecord {
    let mut record = RequestRecord::default();
    record.add_usage(
        ReportedUsage {
            input,
            output,
            ..ReportedUsage::default()
        },
        None,
    );
    record
}

fn calls_of(step: &TraceStep) -> Vec<&str> {
    step.tool_calls
        .iter()
        .flatten()
        .map(|c| c.function_name.as_str())
        .collect()
}

fn results_of(step: &TraceStep) -> usize {
    step.observation.as_ref().map_or(0, |o| o.results.len())
}

#[test]
fn each_model_response_is_its_own_agent_step() {
    let messages = vec![
        Message::user("fix it"),
        call("c1", "A"),
        result("c1", "result A"),
        call("c2", "B"),
        call("c3", "C"),
        result("c2", "result B"),
        result("c3", "result C"),
        Message::assistant("done"),
    ];
    // Each request is recorded at the message count its response ended on.
    let requests = vec![
        (2, request(100, 10)),
        (5, request(200, 20)),
        (8, request(300, 30)),
    ];
    let steps = conversation_steps(
        &ConversationFacts {
            messages: &messages,
            compactions: &[],
            requests: &requests,
            gate_decisions: &[],
            memory_retrievals: &[],
            call_timings: &[],
            origins: &[],
            reasonings: &[],
            system_steps: false,
        },
        &|m| m.clone(),
    );

    let origins: Vec<_> = steps.iter().map(|s| s.source).collect();
    assert_eq!(
        origins,
        [
            StepOrigin::User,
            StepOrigin::Agent,
            StepOrigin::Agent,
            StepOrigin::Agent
        ],
        "one user step and three agent steps"
    );
    assert_eq!(calls_of(&steps[1]), ["A"]);
    assert_eq!(results_of(&steps[1]), 1);
    assert_eq!(
        calls_of(&steps[2]),
        ["B", "C"],
        "parallel calls stay together"
    );
    assert_eq!(results_of(&steps[2]), 2);
    assert!(steps[3].tool_calls.is_none());
    assert_eq!(steps[3].message.as_text(), Some("done"));

    let prompt: Vec<_> = steps[1..]
        .iter()
        .map(|s| s.metrics.as_ref().and_then(|m| m.prompt_tokens))
        .collect();
    assert_eq!(prompt, [Some(100), Some(200), Some(300)]);
    let completion: u64 = steps
        .iter()
        .filter_map(|s| s.metrics.as_ref()?.completion_tokens)
        .sum();
    assert_eq!(
        completion, 60,
        "the steps' tokens sum to the requests' totals"
    );
}
