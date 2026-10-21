// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every task runs in a conversation of its own - one per item inside a
//! `for_each`, so no agent reads another's history - and a loop may run a
//! `do:` body of several tasks per item, each with its own prompt, tools and
//! conversation; a task with checks and no prompt runs its checks without a
//! model turn. Driven at the kernel boundary like the other machine tests.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::{drive, turn};
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessState};
use sven_hsm::{Context, Effect, Event, Machine, ToolCallId};

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

fn tool(effects: &[Effect]) -> (ToolCallId, String, Value) {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::CallTool {
                call_id,
                name,
                args,
                ..
            } => Some((*call_id, name.clone(), args.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a tool call in {effects:?}"))
}

fn gate_report(exit: i64) -> Event {
    // The call id is filled in by `answer`.
    Event::ToolSucceeded {
        call_id: ToolCallId::new(),
        observation: json!({ "exit_code": exit, "timed_out": false, "error": null,
                             "stdout": "", "stderr": "" })
        .to_string()
        .into(),
    }
}

fn answer(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    s: &mut HarnessState,
    id: ToolCallId,
    exit: i64,
) -> Vec<Effect> {
    let Event::ToolSucceeded { observation, .. } = gate_report(exit) else {
        unreachable!()
    };
    drive(
        m,
        ctx,
        s,
        Event::ToolSucceeded {
            call_id: id,
            observation,
        },
    )
}

const TWO_TASKS: &str = r#"
api_version: "2"
name: pair
execution: { limits: { model_turns: 10 } }
workflow:
  - name: plan
    task: "Plan it."
  - name: build
    task: "Build what was planned: {{ tasks.plan.output }}"
outputs:
  built: { type: string, value: "{{ tasks.build.output }}" }
"#;

#[test]
fn each_task_has_a_conversation_of_its_own() {
    let (mut m, mut ctx, mut s, fx) = begin(TWO_TASKS, json!({}));
    let first = llm(&fx);
    assert_eq!(first["thread"], "harness.pair.plan");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.pair.plan", "the plan"),
    );
    let second = llm(&fx);
    assert_eq!(second["thread"], "harness.pair.build");
    assert_eq!(second["instruction"], "Build what was planned: the plan");
}

const SWEEP: &str = r#"
api_version: "2"
name: sweep
inputs: { files: json }
execution: { limits: { model_turns: 40, tool_calls: 40 } }
gates:
  ok: { command: [check, "{{ item_id }}"] }
workflow:
  - name: files
    for_each: { source: { input: files } }
    do:
      - name: audit
        tools: [read_file]
        task: "Audit {{ item }}."
      - name: repair
        tools: [read_file, edit_file]
        task: "Repair {{ item }} given: {{ tasks.audit.output }}"
        verify: [ok]
        repair: { attempts: 1 }
      - name: commit
        verify: [ok]
success:
  work_items: { all_completed: true }
"#;

#[test]
fn a_do_body_runs_its_tasks_per_item_each_in_its_own_conversation() {
    let (mut m, mut ctx, mut s, fx) = begin(SWEEP, json!({ "files": ["a.rs", "b.rs"] }));
    let audit = llm(&fx);
    assert_eq!(audit["thread"], "harness.sweep.audit#0");
    assert_eq!(audit["tools"], json!(["read_file"]));
    assert_eq!(audit["instruction"], "Audit a.rs.");

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.sweep.audit#0", "two problems"),
    );
    let repair = llm(&fx);
    assert_eq!(repair["thread"], "harness.sweep.repair#0");
    assert_eq!(repair["tools"], json!(["read_file", "edit_file"]));
    assert_eq!(repair["instruction"], "Repair a.rs given: two problems");

    // The repair's check fails: its repair turn continues the repair's own
    // conversation, not the audit's.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.sweep.repair#0", "fixed"),
    );
    let (id, name, args) = tool(&fx);
    assert_eq!(name, "shell_gate");
    assert_eq!(args["steps"][0]["argv"], json!(["check", "a.rs"]));
    let fx = answer(&mut m, &mut ctx, &mut s, id, 1);
    let again = llm(&fx);
    assert_eq!(again["thread"], "harness.sweep.repair#0");
    assert_eq!(again["origin"], "repair");

    // Fixed; the commit task is a check task: its gate runs, no model turn.
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.sweep.repair#0", "fixed now"),
    );
    let (id, _, _) = tool(&fx);
    let fx = answer(&mut m, &mut ctx, &mut s, id, 0);
    let (id, name, args) = tool(&fx);
    assert_eq!(name, "shell_gate", "the commit task runs its check at once");
    assert_eq!(args["steps"][0]["argv"], json!(["check", "a.rs"]));
    assert!(
        !fx.iter().any(|e| matches!(e, Effect::CallLlm { .. })),
        "a check task asks no model"
    );

    // The second item starts fresh conversations.
    let fx = answer(&mut m, &mut ctx, &mut s, id, 0);
    let audit = llm(&fx);
    assert_eq!(audit["thread"], "harness.sweep.audit#1");
    assert_eq!(audit["instruction"], "Audit b.rs.");
}

#[test]
fn a_task_may_share_a_named_conversation() {
    let doc = TWO_TASKS
        .replace(
            "  - name: plan\n",
            "  - name: plan\n    conversation: talk\n",
        )
        .replace(
            "  - name: build\n",
            "  - name: build\n    conversation: talk\n",
        );
    let (mut m, mut ctx, mut s, fx) = begin(&doc, json!({}));
    assert_eq!(llm(&fx)["thread"], "harness.pair.talk");
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut s,
        turn("harness.pair.talk", "the plan"),
    );
    assert_eq!(llm(&fx)["thread"], "harness.pair.talk");
}

fn invalid(yaml: &str) -> String {
    match HarnessRunData::new(yaml, json!({ "files": [] })) {
        Ok(_) => panic!("expected the document to be refused"),
        Err(e) => e.to_string(),
    }
}

#[test]
fn bodies_and_check_tasks_are_shaped_as_the_language_says() {
    let no_loop = SWEEP.replace("    for_each: { source: { input: files } }\n", "");
    assert!(
        invalid(&no_loop).contains("`do` is the body of a loop"),
        "{}",
        invalid(&no_loop)
    );

    let prompted_owner = SWEEP.replace(
        "    for_each: { source: { input: files } }\n",
        "    for_each: { source: { input: files } }\n    task: \"x\"\n",
    );
    assert!(invalid(&prompted_owner).contains("takes only"));

    let nested = SWEEP.replace(
        "      - name: commit\n",
        "      - name: commit\n        for_each: { source: { input: files } }\n",
    );
    assert!(invalid(&nested).contains("cannot loop itself"));

    let empty = SWEEP.replace(
        "      - name: commit\n        verify: [ok]\n",
        "      - name: commit\n",
    );
    assert!(invalid(&empty).contains("a task without a prompt runs checks"));
}
