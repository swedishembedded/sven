// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the version 2 workflow language compiles deterministically to a
//! plan whose steps map back to the authored tasks, and every authoring
//! mistake fails offline with an error that names the task or gate.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

/// A three-task workflow: a bootstrap that must build and test, a plan that
/// must leave two files, and a hardening task with a coverage threshold.
const THREE_TASKS: &str = r#"
api_version: "2"
name: hardening
description: Test and harden a repository.
inputs:
  repo: string
agent:
  profile: coding
  instructions: Preserve behavior of {{ inputs.repo }}.
execution:
  limits: { model_turns: 100, tool_calls: 400 }
gates:
  build: { command: [sh, run.sh, build], timeout: 300 }
  test: { command: [sh, run.sh, test] }
  coverage:
    command: [sh, run.sh, coverage]
    metrics:
      line_pct: { file: cov.json, pointer: /line_pct, type: number }
workflow:
  - name: bootstrap
    task: Set up the build.
    verify: [build, test]
    repair: { attempts: 2 }
  - name: plan
    task: Write the plan for {{ inputs.repo }}.
    outputs: [plan.md]
    verify: [{ artifact: plan.md, min_bytes: 10 }]
  - name: harden
    task: "Harden it, following {{ tasks.plan.output }}"
    verify: [test, { metric: coverage.line_pct, gte: 90 }]
    repair: { attempts: 1, prompt: "{{ failure.gate }} failed: {{ failure.stderr }}" }
success:
  gates: [build, test]
  metrics: { coverage.line_pct: { gte: 90 } }
outputs:
  summary: { type: string, value: "{{ tasks.harden.output }}" }
"#;

fn compile(yaml: &str) -> HarnessPlan {
    compile_str(yaml).unwrap_or_else(|e| panic!("expected to compile: {e}"))
}

fn rejected(yaml: &str) -> String {
    compile_str(yaml)
        .expect_err("expected an error")
        .to_string()
}

fn with(replace_from: &str, to: &str) -> String {
    assert!(
        THREE_TASKS.contains(replace_from),
        "{replace_from} not in the base document"
    );
    THREE_TASKS.replacen(replace_from, to, 1)
}

#[test]
fn a_three_task_workflow_compiles_to_a_v2_plan() {
    let plan = compile(THREE_TASKS);
    assert_eq!(plan.api_version, "2");
    assert_eq!(plan.entry, "bootstrap__work");
    assert_eq!(plan.profile.as_deref(), Some("coding"));
    assert_eq!(
        plan.capabilities,
        [
            "DeleteFile",
            "ExecuteShell",
            "ReadFile",
            "RunVerifier",
            "WriteFile"
        ]
    );
    // Every generated step maps back to an authored task or to the workflow.
    assert_eq!(plan.source_map.len(), plan.steps.len());
    assert_eq!(
        plan.source_map["bootstrap__work"].task.as_deref(),
        Some("bootstrap")
    );
    assert_eq!(plan.source_map["complete"].role, "complete");
    // Bounded by construction: the step budget is the plan's size.
    assert_eq!(
        plan.limits.as_ref().and_then(|l| l.max_steps),
        Some(plan.steps.len() as u64)
    );
}

#[test]
fn compiling_twice_and_reordering_keys_gives_one_digest() {
    let reordered = r#"
workflow:
  - task: Set up the build.
    name: bootstrap
    repair: { attempts: 2 }
    verify: [build, test]
  - outputs: [plan.md]
    verify: [{ min_bytes: 10, artifact: plan.md }]
    name: plan
    task: Write the plan for {{ inputs.repo }}.
  - repair: { prompt: "{{ failure.gate }} failed: {{ failure.stderr }}", attempts: 1 }
    verify: [test, { gte: 90, metric: coverage.line_pct }]
    task: "Harden it, following {{ tasks.plan.output }}"
    name: harden
success:
  metrics: { coverage.line_pct: { gte: 90 } }
  gates: [build, test]
gates:
  coverage:
    metrics:
      line_pct: { type: number, pointer: /line_pct, file: cov.json }
    command: [sh, run.sh, coverage]
  test: { command: [sh, run.sh, test] }
  build: { timeout: 300, command: [sh, run.sh, build] }
execution:
  limits: { tool_calls: 400, model_turns: 100 }
agent:
  instructions: Preserve behavior of {{ inputs.repo }}.
  profile: coding
inputs:
  repo: string
outputs:
  summary: { value: "{{ tasks.harden.output }}", type: string }
description: Test and harden a repository.
name: hardening
api_version: "2"
"#;
    let a = compile(THREE_TASKS);
    assert_eq!(a.digest, compile(THREE_TASKS).digest);
    assert_eq!(a.digest, compile(reordered).digest);
}

#[test]
fn changed_semantics_change_the_digest() {
    let base = compile(THREE_TASKS).digest;
    for (what, from, to) in [
        ("repair attempts", "attempts: 2", "attempts: 3"),
        ("a gate command", "run.sh, test]", "run.sh, tests]"),
        ("a prompt", "Set up the build.", "Set up the build quickly."),
        ("a threshold", "gte: 90 }]", "gte: 91 }]"),
    ] {
        assert_ne!(
            compile(&with(from, to)).digest,
            base,
            "{what} must change the digest"
        );
    }
}

#[test]
fn renaming_formatting_and_task_position_keep_other_task_ids_stable() {
    let plan = compile(THREE_TASKS);
    let moved = THREE_TASKS.replacen(
        "workflow:\n",
        "workflow:\n  - name: warmup\n    task: Say hi.\n",
        1,
    );
    let moved = compile(&moved);
    for id in ["bootstrap__work", "plan__work", "harden__work"] {
        assert!(
            plan.steps.contains_key(id) && moved.steps.contains_key(id),
            "{id}"
        );
    }
}

#[test]
fn command_gates_lower_to_gate_steps_with_argv_and_timeout() {
    let plan = compile(THREE_TASKS);
    let step = &plan.steps["bootstrap__v0_build_a0"].primitive;
    let PlanPrimitive::Gate {
        command,
        timeout_secs,
        then,
        on_fail,
        ..
    } = step
    else {
        panic!("expected a gate step, got {step:?}");
    };
    assert_eq!(command.len(), 3);
    assert_eq!(*timeout_secs, 300);
    assert_eq!(then, "bootstrap__v1_test_a0");
    assert_eq!(on_fail, "bootstrap__v0_build_a0_repair");
    // The last attempt has no repair left: its failure ends the run.
    let last = &plan.steps["bootstrap__v0_build_a2"].primitive;
    let PlanPrimitive::Gate { on_fail, .. } = last else {
        panic!("gate expected")
    };
    assert_eq!(on_fail, "bootstrap__v0_build_a2_fail");
}

#[test]
fn a_task_cannot_advance_past_an_unsatisfied_gate() {
    // Every path from a check's failure edge ends in a repair turn that
    // re-runs the checks, or in a `fail` step. Walk them all.
    let plan = compile(THREE_TASKS);
    let mut stack = vec!["bootstrap__v0_build_a0_repair".to_string()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        match &plan.steps[&id].primitive {
            PlanPrimitive::Fail { code, .. } => assert_eq!(code.as_deref(), Some("gate_failed")),
            PlanPrimitive::Agent { then, .. } => stack.push(then.clone()),
            PlanPrimitive::Gate { then, on_fail, .. } => {
                stack.extend([then.clone(), on_fail.clone()]);
            }
            _ => {}
        }
    }
    assert!(
        seen.iter().any(|id| id.ends_with("_fail")),
        "an exhausted task fails the run"
    );
}

#[test]
fn a_metric_check_always_runs_its_command_first() {
    let plan = compile(THREE_TASKS);
    let PlanPrimitive::Gate { then, .. } =
        &plan.steps["harden__v1_coverage_line_pct_a0_run"].primitive
    else {
        panic!("the metric's command must run as a gate step");
    };
    assert_eq!(then, "harden__v1_coverage_line_pct_a0");
    assert!(matches!(
        plan.steps["harden__v1_coverage_line_pct_a0"].primitive,
        PlanPrimitive::Verify { .. }
    ));
}

#[test]
fn authoring_mistakes_fail_with_the_name_of_the_culprit() {
    let cases = [
        (
            "a dangling gate",
            with(
                "verify: [build, test]\n    repair: { attempts: 2 }",
                "verify: [nope]\n    repair: { attempts: 2 }",
            ),
            "references gate `nope`",
        ),
        (
            "a duplicate task name",
            with("name: plan\n", "name: bootstrap\n"),
            "more than one task",
        ),
        (
            "an unknown metric",
            with("coverage.line_pct, gte", "coverage.lines, gte").replace(
                "{ metric: coverage.line_pct, gte: 90 }",
                "{ metric: coverage.lines, gte: 90 }",
            ),
            "declares no metric `lines`",
        ),
        (
            "an unbounded repair",
            with("attempts: 2", "attempts: 99"),
            "repair.attempts 99",
        ),
        (
            "a missing model-turn limit",
            with("model_turns: 100, ", ""),
            "model_turns is required",
        ),
        (
            "an undeclared input",
            with(
                "{{ inputs.repo }}.\n    outputs",
                "{{ inputs.nope }}.\n    outputs",
            ),
            "not declared",
        ),
        (
            "a later task's output",
            with(
                "following {{ tasks.plan.output }}",
                "following {{ tasks.harden.output }}",
            ),
            "has not run by then",
        ),
        (
            "an internal namespace",
            with("{{ tasks.plan.output }}", "{{ facts.task_plan }}"),
            "`facts` and `steps` are internal",
        ),
        (
            "failure outside repair",
            with("task: Set up the build.", "task: \"{{ failure.stderr }}\""),
            "only available in a `repair.prompt`",
        ),
        (
            "a reserved tool",
            with(
                "agent:\n  profile: coding",
                "agent:\n  tools: [shell_gate]\n  profile: coding",
            ),
            "reserved to the harness's own steps",
        ),
        (
            "a gate path escape",
            with(
                "run.sh, build], timeout: 300",
                "run.sh, build], cwd: ../x, timeout: 300",
            ),
            "inside the workspace",
        ),
        (
            "an unused gate",
            with(
                "success:\n  gates: [build, test]",
                "success:\n  gates: [build]",
            )
            .replace("verify: [build, test]\n", "verify: [build]\n")
            .replace("verify: [test, {", "verify: [{"),
            "gate `test` is declared but",
        ),
        (
            "a capability shortfall",
            with(
                "name: hardening",
                "name: hardening\ncapabilities: [ReadFile]",
            ),
            "missing",
        ),
        (
            "a quoted-number api_version",
            THREE_TASKS.replace("api_version: \"2\"", "api_version: 2"),
            "quoted string",
        ),
        (
            "an unknown task key",
            with("name: bootstrap\n", "name: bootstrap\n    verifies: []\n"),
            "unknown field `verifies`",
        ),
        (
            "an unknown top-level key",
            with("name: hardening", "name: hardening\nentry: x"),
            "unknown top-level field `entry`",
        ),
        (
            "a gate of two kinds",
            with(
                "test: { command: [sh, run.sh, test] }",
                "test: { command: [sh], artifact: x }",
            ),
            "exactly one of `command`",
        ),
        (
            "repair with nothing to repair",
            with("name: harden\n", "name: harden\n    outputs: []\n").replace(
                "    verify: [test, { metric: coverage.line_pct, gte: 90 }]\n",
                "",
            ),
            "nothing to repair",
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
fn unsupported_versions_are_named() {
    assert!(rejected("api_version: \"3\"\nname: x\n").contains("supports `1` and `2`"));
}

#[test]
fn version_one_documents_cannot_use_version_two_primitives() {
    let gate = r#"
api_version: "1"
name: g
entry: run
capabilities: [ExecuteShell]
limits: { max_steps: 5 }
steps:
  run: { gate: { command: ["true"], then: ok, on_fail: bad } }
  ok: { return: { value: 1 } }
  bad: { fail: { code: no } }
"#;
    let message = rejected(gate);
    assert!(message.contains("needs `api_version: \"2\"`"), "{message}");
}

#[test]
fn a_long_generated_chain_is_not_mistaken_for_an_unbounded_cycle() {
    let mut yaml = String::from(
        "api_version: \"2\"\nname: long\nexecution: { limits: { model_turns: 1000 } }\nworkflow:\n",
    );
    for i in 0..150 {
        yaml.push_str(&format!("  - name: t{i}\n    task: step {i}\n"));
    }
    let plan = compile(&yaml);
    assert_eq!(plan.steps.len(), 151);
}

/// A workflow with both loops: a per-module task over a backlog file and a
/// coverage task that repeats until the threshold is met.
const LOOPS: &str = r#"
api_version: "2"
name: loops
inputs:
  repo: string
agent: { profile: coding }
execution: { limits: { model_turns: 400 } }
gates:
  build: { command: [sh, run.sh, build] }
  test: { command: [sh, run.sh, test, "{{ item.module }}"] }
  coverage:
    command: [sh, run.sh, coverage]
    metrics:
      line_pct: { file: cov.json, pointer: /line_pct, type: number }
workflow:
  - name: modules
    for_each: { source: { json: { file: backlog.json, pointer: /items } }, id: module }
    task: "Harden {{ item.module }} (number {{ item_index }}) of {{ inputs.repo }}."
    verify: [test]
    repair: { attempts: 1, prompt: "{{ item_id }} broke {{ failure.gate }}" }
  - name: coverage
    until: { metric: coverage.line_pct, gte: 90, max_iterations: 4 }
    task: "Raise coverage (pass {{ iteration }})."
    verify: [build]
success:
  work_items: { all_completed: true }
"#;

#[test]
fn a_for_each_task_lowers_to_an_iterate_step_whose_body_returns_to_it() {
    use sven_harness_spec::{IterateSource, PlanIteration};
    let plan = compile(LOOPS);
    assert_eq!(plan.entry, "modules__iter");
    let PlanPrimitive::Iterate {
        iteration,
        body,
        then,
        store_as,
        ..
    } = &plan.steps["modules__iter"].primitive
    else {
        panic!("an iterate step");
    };
    let PlanIteration::ForEach {
        source,
        id,
        max_items,
    } = iteration
    else {
        panic!("for_each");
    };
    assert!(
        matches!(source, IterateSource::Json(j) if j.file == "backlog.json" && j.pointer == "/items")
    );
    assert_eq!(id.as_deref(), Some("module"));
    assert_eq!(
        *max_items, 500,
        "the default bound is part of the pinned plan"
    );
    assert_eq!(body, "modules__work");
    assert_eq!(then, "coverage__iter");
    assert_eq!(store_as.as_deref(), Some("work_modules"));
    // The body's last edge returns to the loop step.
    let PlanPrimitive::Gate { then, .. } = &plan.steps["modules__v0_test_a1"].primitive else {
        panic!("the last verification is a gate");
    };
    assert_eq!(then, "modules__iter");
}

#[test]
fn an_until_task_checks_its_metric_after_the_body_and_counts_the_misses() {
    use sven_harness_spec::PlanIteration;
    let plan = compile(LOOPS);
    let PlanPrimitive::Iterate {
        iteration,
        body,
        then,
        on_exhausted,
        ..
    } = &plan.steps["coverage__iter"].primitive
    else {
        panic!("an iterate step");
    };
    assert!(matches!(
        iteration,
        PlanIteration::Until { max_iterations: 4 }
    ));
    assert_eq!(body, "coverage__work");
    assert_eq!(then, "success__v0_work_items_modules_a0");
    assert_eq!(on_exhausted, "coverage__exhausted");
    // A pass of the metric leaves the loop; a miss goes back to be counted.
    let PlanPrimitive::Branch {
        when_true,
        when_false,
        ..
    } = &plan.steps["coverage__until__v0_coverage_line_pct_a0_ok"].primitive
    else {
        panic!("the metric predicate is checked by a branch on its verdict");
    };
    assert_eq!(when_true, "success__v0_work_items_modules_a0");
    assert_eq!(when_false, "coverage__iter");
    let PlanPrimitive::Fail { code, .. } = &plan.steps["coverage__exhausted"].primitive else {
        panic!("exhaustion fails the run");
    };
    assert_eq!(code.as_deref(), Some("until_exhausted"));
}

#[test]
fn a_looping_workflow_gets_a_step_bound_that_covers_its_passes() {
    let plan = compile(LOOPS);
    let steps = plan
        .limits
        .as_ref()
        .and_then(|l| l.max_steps)
        .expect("max_steps");
    assert!(
        steps > 500 * 5,
        "500 items of a multi-step body fit: {steps}"
    );
}

#[test]
fn loop_mistakes_fail_with_the_name_of_the_culprit() {
    let with_loops = |from: &str, to: &str| {
        assert!(LOOPS.contains(from), "{from}");
        LOOPS.replacen(from, to, 1)
    };
    let cases = [
        ("both loops", with_loops("    task: \"Harden", "    until: { metric: coverage.line_pct, gte: 1, max_iterations: 2 }\n    task: \"Harden"), "not both"),
        ("an unbounded until", with_loops("max_iterations: 4", "max_iterations: 5000"), "max_iterations"),
        ("item outside a for_each", with_loops("Raise coverage (pass {{ iteration }})", "Raise {{ item_id }}"), "only available inside a `for_each`"),
        ("iteration outside an until", with_loops("Harden {{ item.module }} (number {{ item_index }})", "Harden {{ iteration }}"), "only available inside an `until`"),
        ("an undeclared input source", with_loops("json: { file: backlog.json, pointer: /items }", "input: nope"), "not declared"),
        ("a source outside the workspace", with_loops("file: backlog.json", "file: ../backlog.json"), "inside the workspace"),
        ("a source pointer that is not a pointer", with_loops("pointer: /items", "pointer: items"), "RFC 6901"),
        ("work items without a loop", with("success:\n  gates: [build, test]", "success:\n  work_items: { all_completed: true }\n  gates: [build, test]"), "needs a task with `for_each`"),
        ("an until metric that is not declared", with_loops("metric: coverage.line_pct, gte: 90", "metric: coverage.nope, gte: 90"), "declares no metric `nope`"),
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
fn a_for_each_over_an_input_needs_a_json_input() {
    let yaml = LOOPS
        .replace(
            "json: { file: backlog.json, pointer: /items }",
            "input: modules",
        )
        .replace("  repo: string\n", "  repo: string\n  modules: json\n");
    let plan = compile(&yaml);
    assert_eq!(
        plan.capabilities
            .iter()
            .filter(|c| *c == "ReadFile")
            .count(),
        1,
        "coding already reads"
    );
    let wrong = yaml.replace("modules: json", "modules: string");
    assert!(rejected(&wrong).contains("must be declared `json`"));
}

#[test]
fn coverage_gate_cannot_be_bypassed() {
    // The success condition is the last thing the plan does. Every step
    // reachable from it is a command, a verification, a branch on a
    // verification's verdict, or a terminal: no agent turn, so nothing the
    // model says can change a verdict, and an unmet threshold can only end in
    // `fail`. The figure compared is read after a fresh run of its command.
    let plan = compile(THREE_TASKS);
    let first = plan
        .source_map
        .iter()
        .find(|(_, origin)| origin.role == "check" && origin.task.is_none())
        .map(|(id, _)| id.clone())
        .expect("the plan has a success phase");
    let mut stack = vec![first];
    let mut seen = std::collections::BTreeSet::new();
    let mut outcomes = std::collections::BTreeSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        match &plan.steps[&id].primitive {
            PlanPrimitive::Gate { then, on_fail, .. } => {
                stack.extend([then.clone(), on_fail.clone()])
            }
            PlanPrimitive::Verify { then, .. } => stack.push(then.clone()),
            PlanPrimitive::Branch {
                when_true,
                when_false,
                ..
            } => {
                stack.extend([when_true.clone(), when_false.clone()]);
            }
            PlanPrimitive::Fail { code, .. } => {
                outcomes.insert(code.clone().unwrap_or_default());
            }
            PlanPrimitive::Return { .. } => {
                outcomes.insert("return".to_string());
            }
            other => panic!("`{id}`: the success phase must not contain {other:?}"),
        }
    }
    assert_eq!(
        outcomes,
        ["return".to_string(), "success_check_failed".to_string()].into(),
        "the success phase ends in a return or in its own failure"
    );
    for id in &seen {
        if id.starts_with("success__v") && id.contains("coverage") && id.ends_with("_a0") {
            assert!(
                seen.contains(&format!("{id}_run")),
                "{id}: the figure is read after a fresh run of the command"
            );
        }
    }
}

#[test]
fn a_for_each_task_may_name_its_item_in_file_checks_and_no_other_task_may() {
    use sven_harness_spec::PlanPrimitive;
    let with_report = LOOPS.replace(
        "    verify: [test]\n    repair: { attempts: 1, prompt:",
        "    outputs: [\"reports/{{ item_id }}.md\"]\n    verify: [test]\n    repair: { attempts: 1, prompt:",
    );
    let plan = compile(&with_report);
    let PlanPrimitive::Verify { spec, .. } =
        &plan.steps["modules__v0_reports_item_id_md_a0"].primitive
    else {
        panic!(
            "the output is checked with a verifier: {:?}",
            plan.steps.keys().collect::<Vec<_>>()
        );
    };
    assert_eq!(spec["path"], "reports/{{facts.item_id}}.md");
    // Outside a for_each the path is a literal.
    let outside = THREE_TASKS.replace(
        "outputs: [plan.md]",
        "outputs: [\"plan-{{ inputs.repo }}.md\"]",
    );
    assert!(rejected(&outside).contains("only a `for_each` task's paths may name the item"));
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
            "    thinking: { enabled: false }\n    turn: { max_output_tokens: \"{{ inputs.cap }}\", first_response_timeout: 300 }\n    task: Plan",
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
fn version_one_documents_cannot_set_decoding() {
    let yaml = r#"
api_version: "1"
name: d
entry: ask
limits: { max_steps: 5, max_model_turns: 2 }
steps:
  ask: { predict: { prompt: hi, decoding: { temperature: 0.1 }, then: done } }
  done: { return: { value: 1 } }
"#;
    assert!(rejected(yaml).contains("on a predict step"));
}
