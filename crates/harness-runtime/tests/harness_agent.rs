// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The agent step's tool contract and the delegation depth a run carries.
//!
//! An agent step offers the model exactly its declared tools (or, when it
//! declares none, the session's agent-mode tools), refuses a proposed call
//! outside an explicit list before anything runs, and tells the model why.
//! A run started below the top of a delegation chain enforces only the
//! depth budget that remains.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

mod common;

use common::{drive, start, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessRunState, HarnessState};
use sven_hsm::ids::ToolCallId;
use sven_hsm::{Context, Effect, Event, Machine, ProposedToolCall, ToolCapability};

const THREAD: &str = "harness.worker";

fn agent_harness(tools: &str) -> String {
    format!(
        r#"
api_version: "1"
name: worker
entry: do
limits: {{ max_steps: 100, max_model_turns: 20, max_tool_calls: 20 }}
steps:
  do:
    agent:
      prompt: "Do the work"
{tools}      then: out
  out:
    return:
      value: "{{{{ steps.do.value }}}}"
"#
    )
}

fn llm_request(m: &HarnessMachine) -> Value {
    match m.step_effect().expect("the step emitted an effect") {
        Effect::CallLlm { request } => request,
        other => panic!("expected CallLlm, got {other:?}"),
    }
}

fn llm_requests(effects: &[Effect]) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .collect()
}

fn proposal(name: &str, capability: ToolCapability) -> ProposedToolCall {
    ProposedToolCall {
        call_id: ToolCallId::new(),
        name: name.into(),
        args: json!({}),
        capability,
    }
}

fn proposed(calls: Vec<ProposedToolCall>) -> Event {
    Event::LlmTurnComplete {
        thread: THREAD.into(),
        text: String::new(),
        tool_calls: calls,
    }
}

fn tool_names(effects: &[Effect]) -> Vec<String> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            Effect::CallTool { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_agent_steps_first_turn_offers_its_allowlist() {
    let (m, _ctx, state) = start(
        &agent_harness("      tools: [read_file, write_file]\n"),
        json!({}),
    );
    assert_eq!(state, HarnessState::WaitingForModel);
    assert_eq!(
        llm_request(&m)["tools"],
        json!(["read_file", "write_file"]),
        "the first turn must offer the step's tools, not none"
    );
}

#[test]
fn an_agent_step_without_a_tool_list_offers_the_mode_tools() {
    let (mut m, mut ctx, mut state) = start(&agent_harness(""), json!({}));
    let first = llm_request(&m);
    assert_eq!(first["tools"], json!([]));
    assert_eq!(first["all_tools_mode"], "agent");

    // The continuation turn after a settled tool keeps the same offer.
    let call = proposal("read_file", ToolCapability::ReadFile);
    let id = call.call_id;
    let _ = drive(&mut m, &mut ctx, &mut state, proposed(vec![call]));
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: id,
            observation: json!("ok"),
        },
    );
    let next = &llm_requests(&fx)[0];
    assert_eq!(next["all_tools_mode"], "agent");
}

#[test]
fn a_proposed_call_outside_the_allowlist_is_never_run() {
    let (mut m, mut ctx, mut state) =
        start(&agent_harness("      tools: [read_file]\n"), json!({}));
    let allowed = proposal("read_file", ToolCapability::ReadFile);
    let denied = proposal("write_file", ToolCapability::WriteFile);
    let (allowed_id, denied_id) = (allowed.call_id, denied.call_id);

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        proposed(vec![denied, allowed]),
    );
    assert_eq!(tool_names(&fx), ["read_file"], "only the allowed call runs");
    assert_eq!(HarnessRunState::load(&ctx).counts.tool_calls, 1);

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: allowed_id,
            observation: json!("contents"),
        },
    );
    let requests = llm_requests(&fx);
    assert_eq!(
        requests.len(),
        1,
        "the loop continues once the tool settled"
    );
    let refused = requests[0]["answered_calls"].as_array().expect("refusals");
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0]["call_id"], denied_id.as_uuid().to_string());
    assert!(
        refused[0]["content"]
            .as_str()
            .is_some_and(|r| r.contains("write_file") && r.contains("allowlist")),
        "the model is told why: {refused:?}"
    );
}

#[test]
fn a_turn_of_only_refused_calls_continues_at_once() {
    let (mut m, mut ctx, mut state) =
        start(&agent_harness("      tools: [read_file]\n"), json!({}));
    let denied = proposal("write_file", ToolCapability::WriteFile);
    let denied_id = denied.call_id;

    let fx = drive(&mut m, &mut ctx, &mut state, proposed(vec![denied]));
    assert!(tool_names(&fx).is_empty(), "nothing runs: {fx:?}");
    let requests = llm_requests(&fx);
    assert_eq!(requests.len(), 1, "exactly one continuation turn: {fx:?}");
    assert_eq!(
        requests[0]["answered_calls"][0]["call_id"],
        denied_id.as_uuid().to_string()
    );
    assert_eq!(state, HarnessState::WaitingForModel);
    assert_eq!(HarnessRunState::load(&ctx).counts.tool_calls, 0);

    // The loop is alive: a text answer finishes the step.
    let _ = drive(&mut m, &mut ctx, &mut state, turn(THREAD, "gave up"));
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(m.outcome(), Some(json!("gave up")));
}

#[test]
fn a_delegate_descriptor_carries_the_childs_depth() {
    let yaml = r#"
api_version: "1"
name: caller
entry: go
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3 }
steps:
  go:
    delegate: { target: helper, input: {}, then: out }
  out:
    return: { value: "done" }
"#;
    let (m, _ctx, _state) = start(yaml, json!({}));
    let Some(Effect::InstantiateSubmachine { descriptor, .. }) = m.step_effect() else {
        panic!("delegate emits InstantiateSubmachine");
    };
    assert_eq!(descriptor["depth"], 2);
}

#[test]
fn a_run_started_deeper_enforces_the_remaining_depth_budget() {
    let yaml = r#"
api_version: "1"
name: caller
entry: go
capabilities: [SpawnChild]
limits: { max_steps: 50, max_delegate_depth: 3 }
steps:
  go:
    delegate: { target: helper, input: {}, then: out }
  out:
    return: { value: "done" }
"#;
    let run = HarnessRunData::new(yaml, json!({}))
        .expect("valid harness")
        .with_delegate_depth(3);
    assert_eq!(run.delegate_depth(), 3);
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));

    assert_eq!(state, HarnessState::Failed);
    assert_eq!(m.outcome().expect("outcome")["code"], "delegate_depth");
}
