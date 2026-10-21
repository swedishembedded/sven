// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: run variables. A task names the variables it may see; its agent
//! reads and sets them through tools the machine answers itself, a value is
//! checked against its declaration, later templates read it, a `required`
//! variable left unset earns reminders in the task's own conversation and
//! then fails the run, and an `item` variable starts every item unset.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessState};
use sven_hsm::{Context, Effect, Event, Machine, ProposedToolCall, ToolCallId, ToolCapability};

fn begin(yaml: &str, inputs: Value) -> (HarnessMachine, Context, HarnessState, Vec<Effect>) {
    let run = HarnessRunData::new(yaml, inputs).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    let fx = drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state, fx)
}

fn llm(effects: &[Effect]) -> Value {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a model call in {effects:?}"))
}

/// The model calls `tool` with `args` in `thread`.
fn calls(thread: &str, tool: &str, args: Value) -> (Event, ToolCallId) {
    let id = ToolCallId::new();
    (
        Event::LlmTurnComplete {
            thread: thread.into(),
            text: String::new(),
            tool_calls: vec![ProposedToolCall {
                call_id: id,
                name: tool.into(),
                args,
                capability: ToolCapability::ReadFile,
            }],
        },
        id,
    )
}

/// What the machine answered `id` with in the next request.
fn answer_to(request: &Value, id: ToolCallId) -> String {
    request["answered_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["call_id"] == id.as_uuid().to_string())
        .and_then(|a| a["content"].as_str())
        .unwrap_or_else(|| panic!("an answer to the call in {request}"))
        .to_string()
}

const DISCOVER: &str = r#"
api_version: "2"
name: disc
agent: { tools: [read_file] }
execution: { limits: { model_turns: 20, tool_calls: 20 } }
variables:
  checks:
    type: json
    description: how to build and test
    schema: { type: object, required: [test], properties: { test: { type: string } } }
  note: { type: string }
workflow:
  - name: explore
    task: "Find out how to test."
    vars: { checks: required, note: read }
  - name: use
    task: "Run {{ vars.checks.test }}."
outputs:
  test: { type: string, value: "{{ vars.checks.test }}" }
"#;

#[test]
fn an_agent_sets_a_variable_that_later_templates_read() {
    let (mut m, mut ctx, mut s, fx) = begin(DISCOVER, json!({}));
    let first = llm(&fx);
    let offered: Vec<String> = serde_json::from_value(first["tools"].clone()).unwrap();
    assert_eq!(
        offered,
        [
            "read_file",
            "harness_vars_list",
            "harness_vars_get",
            "harness_vars_set"
        ]
    );
    let thread = "harness.disc.explore";

    // Wrong shape: refused, with the reason; nothing is stored.
    let (event, id) = calls(
        thread,
        "harness_vars_set",
        json!({ "name": "checks", "value": { "tset": 1 } }),
    );
    let fx = drive(&mut m, &mut ctx, &mut s, event);
    let next = llm(&fx);
    assert!(
        answer_to(&next, id).contains("lacks the required field `test`"),
        "{next}"
    );
    assert!(
        fx.iter().all(|e| !matches!(e, Effect::CallTool { .. })),
        "never an executor call"
    );

    // Read-only: refused.
    let (event, id) = calls(
        thread,
        "harness_vars_set",
        json!({ "name": "note", "value": "x" }),
    );
    let fx = drive(&mut m, &mut ctx, &mut s, event);
    assert!(answer_to(&llm(&fx), id).contains("read-only"));

    // Right shape, sent as text: stored as the structure.
    let (event, id) = calls(
        thread,
        "harness_vars_set",
        json!({ "name": "checks", "value": "{\"test\": \"cargo test\"}" }),
    );
    let fx = drive(&mut m, &mut ctx, &mut s, event);
    assert_eq!(answer_to(&llm(&fx), id), "ok: `checks` is set");

    let (event, id) = calls(thread, "harness_vars_list", json!({}));
    let fx = drive(&mut m, &mut ctx, &mut s, event);
    let listed: Value = serde_json::from_str(&answer_to(&llm(&fx), id)).unwrap();
    assert_eq!(listed[0]["name"], "checks");
    assert_eq!(listed[0]["access"], "write");
    assert_eq!(listed[0]["set"], true);
    assert_eq!(listed[1]["name"], "note");
    assert_eq!(listed[1]["access"], "read");

    // The task ends; the next one reads the value in its prompt.
    let fx = drive(&mut m, &mut ctx, &mut s, turn(thread, "done"));
    let next = llm(&fx);
    assert_eq!(next["thread"], "harness.disc.use");
    assert_eq!(next["instruction"], "Run cargo test.");
    assert_eq!(
        next["tools"],
        json!(["read_file"]),
        "a task naming no variable gets no variable tools"
    );
    // Every set is on the trace, refused or stored, with the agent's answer.
    let sets: Vec<bool> = m
        .trace(&ctx)
        .iter()
        .filter_map(|r| match r {
            sven_harness_runtime::TraceRecord::VariableSet { stored, .. } => Some(*stored),
            _ => None,
        })
        .collect();
    assert_eq!(sets, [false, false, true]);
}

#[test]
fn a_required_variable_left_unset_is_asked_for_and_then_fails_the_run() {
    let (mut m, mut ctx, mut s, _) = begin(DISCOVER, json!({}));
    let thread = "harness.disc.explore";
    let mut fx = drive(&mut m, &mut ctx, &mut s, turn(thread, "I looked around."));
    for _ in 0..2 {
        let reminder = llm(&fx);
        assert_eq!(
            reminder["thread"], thread,
            "the reminder continues the task's conversation"
        );
        assert_eq!(reminder["origin"], "variables");
        assert!(reminder["instruction"]
            .as_str()
            .unwrap()
            .contains("`checks`"));
        fx = drive(&mut m, &mut ctx, &mut s, turn(thread, "Sure."));
    }
    let outcome = m.outcome().expect("the run ended");
    assert_eq!(outcome["code"], "missing_variable", "{outcome}");
    assert!(
        outcome["message"].as_str().unwrap().contains("`checks`"),
        "{outcome}"
    );
}

const ITEMS: &str = r#"
api_version: "2"
name: items
inputs: { files: json }
agent: { tools: [read_file] }
execution: { limits: { model_turns: 20, tool_calls: 20 } }
variables:
  findings: { type: string, scope: item }
workflow:
  - name: files
    for_each: { source: { input: files } }
    do:
      - name: audit
        task: "Audit {{ item }}."
        vars: { findings: write }
      - name: fix
        task: "Fix {{ item }}: {{ vars.findings }}"
        vars: { findings: read }
"#;

#[test]
fn an_item_variable_starts_every_item_unset() {
    let (mut m, mut ctx, mut s, _) = begin(ITEMS, json!({ "files": ["a", "b"] }));
    let (event, _) = calls(
        "harness.items.audit#0",
        "harness_vars_set",
        json!({ "name": "findings", "value": "dead code" }),
    );
    drive(&mut m, &mut ctx, &mut s, event);
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.items.audit#0", "done"),
    );
    assert_eq!(llm(&fx)["instruction"], "Fix a: dead code");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.items.fix#0", "fixed"),
    );
    assert_eq!(llm(&fx)["thread"], "harness.items.audit#1");
    let (event, id) = calls(
        "harness.items.audit#1",
        "harness_vars_get",
        json!({ "name": "findings" }),
    );
    let fx = drive(&mut m, &mut ctx, &mut s, event);
    assert_eq!(answer_to(&llm(&fx), id), "`findings` is not set yet");
}

fn refused(yaml: &str) -> String {
    match HarnessRunData::new(yaml, json!({})) {
        Ok(_) => panic!("expected the document to be refused"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn grants_are_checked_against_the_declarations() {
    let undeclared = DISCOVER.replace("note: read", "other: read");
    assert!(refused(&undeclared).contains("not declared under `variables`"));
    let planner = DISCOVER.replace(
        "    task: \"Find out how to test.\"\n",
        "    task: \"Find out how to test.\"\n    model_only: true\n",
    );
    assert!(refused(&planner).contains("only an agent task can set a variable"));
    let no_tools = DISCOVER.replace("agent: { tools: [read_file] }\n", "");
    assert!(refused(&no_tools).contains("needs a `tools` list"));
    let listed = DISCOVER.replace("tools: [read_file]", "tools: [read_file, harness_vars_set]");
    assert!(refused(&listed).contains("offered automatically"));
    let typo = DISCOVER.replace("{{ vars.checks.test }}.\"", "{{ vars.chekcs.test }}.\"");
    assert!(refused(&typo).contains("variable `chekcs`"));
}

const CAPTURED: &str = r#"
api_version: "2"
name: cap
agent: { tools: [read_file] }
execution: { limits: { model_turns: 20, tool_calls: 20 } }
variables:
  files: { type: json }
  checks: { type: json }
gates:
  inventory: { run: "git ls-files", capture: files }
  build: { run: "{{ vars.checks.build }}" }
workflow:
  - name: explore
    task: "Explore."
    vars: { checks: required }
    verify: [inventory, build]
  - name: each
    for_each: { source: { variable: files }, id: file }
    task: "Work on {{ item.file }}."
"#;

fn report(id: ToolCallId, exit: i64, stdout: &str) -> Event {
    Event::ToolSucceeded {
        call_id: id,
        observation: json!({ "exit_code": exit, "timed_out": false, "error": null,
                             "stdout": stdout, "stderr": "" })
        .to_string()
        .into(),
    }
}

fn gate_call(effects: &[Effect]) -> (ToolCallId, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallTool { call_id, args, .. } => Some((*call_id, args.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a gate call in {effects:?}"))
}

#[test]
fn a_gate_captures_its_output_into_a_variable_a_loop_then_iterates() {
    let (mut m, mut c, mut s, _) = begin(CAPTURED, json!({}));
    let (event, _) = calls(
        "harness.cap.explore",
        "harness_vars_set",
        json!({ "name": "checks", "value": { "build": "make" } }),
    );
    drive(&mut m, &mut c, &mut s, event);
    let fx = drive(&mut m, &mut c, &mut s, turn("harness.cap.explore", "done"));
    let (id, _) = gate_call(&fx);
    let fx = drive(
        &mut m,
        &mut c,
        &mut s,
        report(id, 0, "[{\"file\": \"a.rs\"}, {\"file\": \"b.rs\"}]\n"),
    );
    // The build gate runs the command the agent published.
    let (id, args) = gate_call(&fx);
    assert_eq!(args["steps"][0]["argv"][4], "make", "{args}");
    let fx = drive(&mut m, &mut c, &mut s, report(id, 0, ""));
    assert_eq!(llm(&fx)["instruction"], "Work on a.rs.");
    let fx = drive(&mut m, &mut c, &mut s, turn("harness.cap.each#0", "ok"));
    assert_eq!(llm(&fx)["instruction"], "Work on b.rs.");
}

#[test]
fn output_that_is_not_the_variables_type_fails_the_gate() {
    let (mut m, mut c, mut s, _) = begin(
        &CAPTURED.replace(
            "    verify: [inventory, build]\n",
            "    verify: [inventory, build]\n    repair: { attempts: 1 }\n",
        ),
        json!({}),
    );
    let (event, _) = calls(
        "harness.cap.explore",
        "harness_vars_set",
        json!({ "name": "checks", "value": { "build": "make" } }),
    );
    drive(&mut m, &mut c, &mut s, event);
    let fx = drive(&mut m, &mut c, &mut s, turn("harness.cap.explore", "done"));
    let (id, _) = gate_call(&fx);
    let fx = drive(&mut m, &mut c, &mut s, report(id, 0, "a.rs\nb.rs\n"));
    let repair = llm(&fx);
    assert_eq!(
        repair["origin"], "repair",
        "the capture failure is a failed check: {repair}"
    );
}

#[test]
fn a_gate_may_not_read_a_variable_a_later_agent_can_set() {
    let later = CAPTURED.replace(
        "    task: \"Work on {{ item.file }}.\"\n",
        "    task: \"Work on {{ item.file }}.\"\n    vars: { checks: write }\n",
    );
    let error = refused(&later);
    assert!(
        error.contains(
            "gate `build` reads variable `checks`, which task `each` can set after task `explore`"
        ),
        "{error}"
    );
    let undeclared = CAPTURED.replace("capture: files", "capture: nothing");
    assert!(refused(&undeclared).contains("capture names `nothing`"));
}
