// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: roles in a workflow - model-only tasks with their own conversation,
//! system message and schema, and the sampling, thinking and response-bound
//! settings that differ from task to task.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

fn compile(yaml: &str) -> HarnessPlan {
    compile_str(yaml).unwrap_or_else(|e| panic!("expected to compile: {e}"))
}

fn rejected(yaml: &str) -> String {
    compile_str(yaml)
        .expect_err("expected an error")
        .to_string()
}

/// A planner that answers in a schema, a worker with its own decoding, and a
/// supervisor that reads the plan: the roles of a multi-agent port.
const ROLES: &str = r#"
api_version: "2"
name: roles
agent:
  profile: coding
  sampling: { temperature: 0.7, top_p: 0.9 }
  thinking: { enabled: true, budget: 4096 }
execution: { limits: { model_turns: 50 } }
gates:
  leak: { command: [sh, leak.sh], timeout: 30 }
workflow:
  - name: plan
    model_only: true
    schema:
      type: object
      required: [steps]
      properties: { steps: { type: array } }
    sampling: { temperature: 0.1, seed: 7 }
    thinking: { enabled: false }
    task: Plan the work as JSON.
  - name: work
    task: "Do {{ tasks.plan.output.steps }}."
    sampling: { top_k: 20 }
  - name: supervise
    model_only: true
    task: "Say what {{ tasks.work.output }} got wrong."
    verify: [leak]
    repair: { attempts: 1 }
outputs:
  verdict: { type: string, value: "{{ tasks.supervise.output }}" }
"#;

#[test]
fn a_model_only_task_is_one_predict_turn_whose_answer_later_tasks_read() {
    let plan = compile(ROLES);
    let PlanPrimitive::Predict {
        schema, store_as, ..
    } = &plan.steps["plan__work"].primitive
    else {
        panic!("a model_only task lowers to a predict step");
    };
    assert_eq!(schema.as_deref(), Some("plan"));
    assert_eq!(store_as.as_deref(), Some("task_plan"));
    assert!(plan
        .schemas
        .as_ref()
        .is_some_and(|s| s["plan"]["type"] == "object"));
    // A field of the structured answer is addressable; the worker reads it.
    let PlanPrimitive::Agent { prompt, .. } = &plan.steps["work__work"].primitive else {
        panic!("an agent step");
    };
    assert!(String::from(prompt.clone()).contains("{{facts.task_plan.steps}}"));
    // The repair of a model_only task is a model turn too, not an agent loop.
    assert!(matches!(
        plan.steps["supervise__v0_leak_a0_repair"].primitive,
        PlanPrimitive::Predict { .. }
    ));
}

#[test]
fn sampling_and_thinking_are_set_per_step_with_the_task_over_the_agent() {
    let plan = compile(ROLES);
    let decoding = |id: &str| match &plan.steps[id].primitive {
        PlanPrimitive::Predict { decoding, .. } | PlanPrimitive::Agent { decoding, .. } => {
            serde_json::to_value(decoding).expect("serializes")
        }
        other => panic!("{id}: {other:?}"),
    };
    assert_eq!(
        decoding("plan__work"),
        serde_json::json!({
            "temperature": 0.1, "top_p": 0.9, "seed": 7,
            "thinking": false, "thinking_budget": 4096
        }),
        "the task's settings replace the agent's, field by field"
    );
    assert_eq!(decoding("work__work")["top_k"], 20);
    assert_eq!(decoding("work__work")["temperature"], 0.7);
    assert_eq!(decoding("work__work")["thinking"], true);
    // Repair turns decode like the task they repair.
    assert_eq!(
        decoding("supervise__v0_leak_a0_repair"),
        decoding("supervise__work")
    );
    // A different setting is a different program.
    let other = ROLES.replace("temperature: 0.1", "temperature: 0.2");
    assert_ne!(compile(&other).digest, plan.digest);
}

#[test]
fn a_setting_may_be_an_input_and_must_be_a_declared_one() {
    let with_inputs = ROLES
        .replace("name: roles
", "name: roles
inputs: { seed: number, cap: number }
")
        .replace("seed: 7", "seed: \"{{ inputs.seed }}\"")
        .replace(
            "    thinking: { enabled: false }\n    task: Plan",
            "    thinking: { enabled: false }\n    turn: { max_output_tokens: \"{{ inputs.cap }}\", first_response_timeout: 300, deadline: 900 }\n    task: Plan",
        );
    let plan = compile(&with_inputs);
    let PlanPrimitive::Predict { decoding, .. } = &plan.steps["plan__work"].primitive else {
        panic!("predict");
    };
    assert_eq!(decoding.seed, Some(serde_json::json!("{{ inputs.seed }}")));
    assert_eq!(
        decoding.first_response_timeout,
        Some(serde_json::json!(300))
    );
    assert_eq!(decoding.deadline, Some(serde_json::json!(900)));
    let undeclared = with_inputs.replace(
        "inputs: { seed: number, cap: number }",
        "inputs: { cap: number }",
    );
    assert!(rejected(&undeclared).contains("not a declared input"));
    let prose = with_inputs.replace("\"{{ inputs.cap }}\"", "\"lots {{ inputs.cap }}\"");
    assert!(rejected(&prose).contains("single"));
}

#[test]
fn decoding_and_role_mistakes_fail_with_the_name_of_the_culprit() {
    let cases = [
        (
            "a temperature out of range",
            ROLES.replace("temperature: 0.7", "temperature: 3"),
            "temperature `3` is out of range",
        ),
        (
            "a top_p of zero",
            ROLES.replace("top_p: 0.9", "top_p: 0"),
            "top_p `0` is out of range",
        ),
        (
            "a zero budget",
            ROLES.replace("budget: 4096", "budget: 0"),
            "thinking_budget `0` is out of range",
        ),
        (
            "an unknown sampling key",
            ROLES.replace("top_k: 20", "top_q: 20"),
            "unknown field `top_q`",
        ),
        (
            "a schema without model_only",
            ROLES.replace(
                "  - name: work\n",
                "  - name: work\n    schema: { type: object }\n",
            ),
            "goes with `model_only: true`",
        ),
        (
            "tools on a model_only task",
            ROLES.replace(
                "    model_only: true\n    task: \"Say",
                "    model_only: true\n    tools: [read_file]\n    task: \"Say",
            ),
            "without tools",
        ),
        (
            "a field of an unstructured output",
            ROLES.replace("{{ tasks.work.output }}", "{{ tasks.work.output.x }}"),
            "has no `schema`",
        ),
    ];
    for (what, yaml, expect) in cases {
        let message = rejected(&yaml);
        assert!(
            message.contains(expect),
            "{what}: `{message}` should mention `{expect}`"
        );
    }
}

#[test]
fn a_task_names_the_model_role_that_answers_it_and_the_agent_sets_the_default() {
    let yaml = ROLES
        .replace(
            "agent:\n  profile: coding",
            "agent:\n  model: worker\n  profile: coding",
        )
        .replace(
            "    model_only: true\n    schema",
            "    model_only: true\n    model: planner\n    schema",
        );
    let plan = compile(&yaml);
    let role = |id: &str| match &plan.steps[id].primitive {
        PlanPrimitive::Predict { role, .. } | PlanPrimitive::Agent { role, .. } => role.clone(),
        other => panic!("{id}: {other:?}"),
    };
    assert_eq!(role("plan__work").as_deref(), Some("planner"));
    assert_eq!(role("work__work").as_deref(), Some("worker"));
    // Repair turns are answered by their task's role.
    assert_eq!(
        role("supervise__v0_leak_a0_repair").as_deref(),
        Some("worker")
    );
    // An endpoint or a key is not a role.
    let message = rejected(&yaml.replace("model: planner", "model: \"http://host/v1\""));
    assert!(message.contains("not a role name"), "{message}");
}
