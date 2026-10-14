// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A task's scripted model: the mock-response file and the conversation file
//! that make `sven --model mock` replay a segment.
//!
//! The scripted model cannot reason. It recognises each turn by its marker in
//! the user text and answers with the turn's tool calls, then its reply. What
//! a suite can measure is therefore what the harness did with a fixed model
//! behaviour, never whether a model would have succeeded.

use std::path::Path;

use serde::Serialize;

use crate::task::{ScriptedCall, Segment, Task};

#[derive(Serialize)]
struct MockFile<'a> {
    request_log: String,
    responses: Vec<Rule<'a>>,
}

#[derive(Serialize)]
struct Rule<'a> {
    match_type: &'static str,
    #[serde(skip_serializing_if = "str::is_empty")]
    pattern: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<Call<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_tool_reply: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply: Option<&'a str>,
}

#[derive(Serialize)]
struct Call<'a> {
    id: &'a str,
    tool: &'a str,
    args: &'a serde_json::Value,
}

fn call(c: &ScriptedCall) -> Call<'_> {
    Call {
        id: &c.id,
        tool: &c.tool,
        args: &c.args,
    }
}

/// The mock-response YAML for `segment`, logging every request to
/// `request_log`.
#[must_use]
pub fn render_mock(segment: &Segment, request_log: &Path) -> String {
    let mut responses: Vec<Rule<'_>> = segment
        .turns
        .iter()
        .chain(&segment.children)
        .map(|turn| Rule {
            match_type: "contains",
            pattern: &turn.marker,
            tool_calls: turn.calls.iter().map(call).collect(),
            after_tool_reply: (!turn.calls.is_empty()).then_some(turn.reply.as_str()),
            reply: turn.calls.is_empty().then_some(turn.reply.as_str()),
        })
        .collect();
    responses.push(Rule {
        match_type: "default",
        pattern: "",
        tool_calls: Vec::new(),
        after_tool_reply: None,
        reply: Some("No scripted turn matches this instruction."),
    });
    serde_yaml::to_string(&MockFile {
        request_log: request_log.display().to_string(),
        responses,
    })
    .expect("a mock script serializes")
}

/// The conversation file for segment `index` of `task`: one `##` step per
/// turn, each of which sven sends as a user message.
#[must_use]
pub fn render_conversation(task: &Task, index: usize) -> String {
    let mut out = format!("# {} segment {index}\n", task.id);
    for (n, turn) in task.segments[index].turns.iter().enumerate() {
        out.push_str(&format!("\n## Turn {:02}\n\n{}\n", n + 1, turn.user));
    }
    out
}
