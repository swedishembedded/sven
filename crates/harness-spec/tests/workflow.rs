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
            "reserved to gates",
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
