// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a gate can be given a task's output on standard input, its latest
//! result can be read as data after it ran (exit status, output, the output
//! parsed as JSON), a check can be recorded without being required, and a
//! workflow can end a failed attempt by returning what it produced.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: gates-as-data
inputs: { target: string }
execution: { limits: { model_turns: 20 } }
gates:
  check_plan:
    command: [checker, plan, "--target", "{{ inputs.target }}"]
    stdin: "{{ tasks.plan.output }}"
  judge: { command: [checker, judge] }
workflow:
  - name: plan
    model_only: true
    task: Write the plan.
    verify: [check_plan]
    repair: { attempts: 2, prompt: "Refused: {{ failure.stdout }}" }
  - name: work
    task: "Do {{ tasks.plan.output }}"
    verify: [{ gate: judge, required: false }]
  - name: report
    model_only: true
    task: "The judge said {{ gates.judge.json.classes }} (exit {{ gates.judge.exit_code }}, passed {{ gates.judge.passed }})."
outputs:
  verdict: { type: json, value: "{{ gates.judge.json }}" }
"#;

fn compile(yaml: &str) -> HarnessPlan {
    compile_str(yaml).unwrap_or_else(|e| panic!("expected to compile: {e}"))
}

fn rejected(yaml: &str) -> String {
    compile_str(yaml)
        .expect_err("expected an error")
        .to_string()
}

#[test]
fn a_gate_reads_a_tasks_output_on_standard_input_and_keeps_its_result_for_later() {
    let plan = compile(DOC);
    let PlanPrimitive::Gate {
        stdin, store_as, ..
    } = &plan.steps["plan__v0_check_plan_a0"].primitive
    else {
        panic!("a gate step");
    };
    assert_eq!(
        String::from(stdin.clone().expect("stdin")),
        "{{facts.task_plan}}"
    );
    assert_eq!(store_as.as_deref(), Some("gate_check_plan"));
    // The latest result of a gate is addressable as data.
    let PlanPrimitive::Predict { prompt, .. } = &plan.steps["report__work"].primitive else {
        panic!("a predict step");
    };
    let text = String::from(prompt.clone());
    assert!(text.contains("{{facts.gate_judge.json.classes}}"), "{text}");
    assert!(text.contains("{{facts.gate_judge.exit_code}}"), "{text}");
}

#[test]
fn a_check_that_is_not_required_never_repairs_or_fails_the_task() {
    let plan = compile(DOC);
    let PlanPrimitive::Gate { then, on_fail, .. } = &plan.steps["work__v0_judge_a0"].primitive
    else {
        panic!("a gate step");
    };
    assert_eq!(on_fail, then, "pass or fail, the run goes on");
    assert!(!plan
        .steps
        .keys()
        .any(|id| id.starts_with("work__v0_judge") && id.ends_with("_fail")));
}

#[test]
fn reading_the_result_of_a_gate_that_does_not_exist_is_an_error() {
    let message = rejected(&DOC.replace("gates.judge.exit_code", "gates.nope.exit_code"));
    assert!(message.contains("gate `nope`"), "{message}");
    let artifact = DOC
        .replace(
            "  judge: { command: [checker, judge] }",
            "  judge: { command: [checker, judge] }\n  file: { artifact: a.txt }",
        )
        .replace("gates.judge.passed", "gates.file.passed")
        .replace(
            "verify: [{ gate: judge, required: false }]",
            "verify: [{ gate: judge, required: false }, file]",
        );
    let message = rejected(&artifact);
    assert!(message.contains("not a declared command gate"), "{message}");
}

#[test]
fn only_a_command_gate_can_be_recorded_without_being_required() {
    let yaml = DOC
        .replace(
            "  judge: { command: [checker, judge] }",
            "  judge: { artifact: a.txt }",
        )
        .replace("{{ gates.judge.json.classes }}", "x")
        .replace("{{ gates.judge.exit_code }}", "x")
        .replace("{{ gates.judge.passed }}", "x")
        .replace("{{ gates.judge.json }}", "x");
    assert!(rejected(&yaml).contains("only a command gate can be recorded"));
}

#[test]
fn a_task_cannot_feed_a_gate_the_output_of_a_task_that_has_not_run() {
    let message = rejected(&DOC.replace(
        "{{ tasks.plan.output }}\"\n  judge",
        "{{ tasks.report.output }}\"\n  judge",
    ));
    assert!(message.contains("has not run by then"), "{message}");
}

#[test]
fn with_on_failure_return_a_failed_check_returns_the_outputs_marked_failed() {
    let plan = compile(&format!("on_failure: return\n{DOC}"));
    let ends: Vec<&PlanPrimitive> = plan
        .steps
        .values()
        .map(|s| &s.primitive)
        .filter(|p| matches!(p, PlanPrimitive::Return { .. } | PlanPrimitive::Fail { .. }))
        .collect();
    assert!(
        ends.iter()
            .all(|p| matches!(p, PlanPrimitive::Return { .. })),
        "no step ends the run as failed: {ends:?}"
    );
    let PlanPrimitive::Return { value } = &plan.steps["plan__v0_check_plan_a2_fail"].primitive
    else {
        panic!("the exhausted check returns");
    };
    assert_eq!(value["status"], "failed");
    assert_eq!(value["failure"]["code"], "gate_failed");
    assert!(value["failure"]["message"]
        .as_str()
        .unwrap()
        .contains("check_plan"));
    assert_eq!(
        value["verdict"],
        "{{ gates.judge.json }}"
            .replace("gates.judge", "facts.gate_judge")
            .replace(' ', "")
    );
    let PlanPrimitive::Return { value } = &plan.steps["complete"].primitive else {
        panic!("complete")
    };
    assert_eq!(value["status"], "completed");
}

const STOPS: &str = r#"
api_version: "2"
name: stops
inputs: { calls: number, per: number }
execution: { limits: { model_turns: 40, tool_calls: "{{ inputs.calls }}" } }
workflow:
  - name: work
    allowance: "{{ inputs.calls }}"
    max_repeated_results: 4
    task: Do it.
    follow_ups:
      - task: Go on.
        when: { last_turn: stopped_without_conclusion }
        max: 2
        allowance: "{{ inputs.per }}"
outputs:
  stop: { type: string, value: "{{ tasks.work.stop }}" }
"#;

#[test]
fn follow_ups_repeat_while_their_condition_holds_and_budgets_come_from_inputs() {
    let plan = compile(STOPS);
    assert_eq!(
        plan.limits
            .as_ref()
            .and_then(|l| l.max_tool_calls_input.as_deref()),
        Some("calls")
    );
    // Two copies of the follow-up, each behind its own check of the last turn.
    for id in ["work__f0", "work__f0_when", "work__f0_1", "work__f0_1_when"] {
        assert!(plan.steps.contains_key(id), "{id}");
    }
    let PlanPrimitive::Branch { on, .. } = &plan.steps["work__f0_when"].primitive else {
        panic!("a branch");
    };
    assert_eq!(
        serde_json::Value::from(on.clone()),
        serde_json::json!({ "eq": "facts.stop_work", "value": "stopped_without_conclusion" })
    );
    let PlanPrimitive::Agent {
        allowance,
        pool,
        max_repeated_results,
        ..
    } = &plan.steps["work__work"].primitive
    else {
        panic!("an agent step");
    };
    assert!(allowance.is_some());
    assert_eq!(pool.as_deref(), Some("calls_left_work"));
    assert_eq!(*max_repeated_results, Some(4));
    // Only the first of the repeated turns resets the allowance.
    let PlanPrimitive::Agent { allowance, .. } = &plan.steps["work__f0_1"].primitive else {
        panic!("an agent step");
    };
    assert!(allowance.is_none());
}

#[test]
fn budgets_must_name_declared_inputs_and_follow_ups_stay_within_eight() {
    assert!(rejected(&STOPS.replace("inputs.per", "inputs.nope")).contains("allowance"));
    assert!(rejected(&STOPS.replace("max: 2", "max: 9")).contains("max must be within"));
    assert!(rejected(&STOPS.replace(
        "tool_calls: \"{{ inputs.calls }}\"",
        "tool_calls: \"{{ inputs.nope }}\""
    ))
    .contains("allowance"));
}
