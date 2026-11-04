// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a command gate may be a script. `run:` is one multiline script or a
//! list of named steps run in order, under `bash -eo pipefail` (or `sh -e`);
//! `env:` hands values to the command as variables, so data never has to be
//! spliced into script text. Exactly one of `command` and `run` is written.
//! The script is a template like any argument, and a failed step is named in
//! `gates.<name>.failed_step` and `failure.failed_step`.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning build and test
//! scripts into trusted checks then you can procure our services by sending
//! an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: scripted
inputs: { target: string }
execution: { limits: { model_turns: 8 } }
gates:
  build:
    run: |
      make "{{ inputs.target }}"
      test -f out
    env: { TARGET: "{{ inputs.target }}", PAD: "{{ run.scratchpad }}" }
    timeout: 120
  staged:
    shell: sh
    run:
      - { name: compile, script: "cc -o t {{ inputs.target }}.c" }
      - { name: test, script: "./t" }
  plain:
    command: [checker]
    env: { MODE: strict }
workflow:
  - name: work
    task: Do it.
    verify: [build, staged, plain]
    repair:
      attempts: 1
      prompt: "Step {{ failure.failed_step }} of {{ failure.gate }} failed: {{ gates.build.failed_step }}"
"#;

fn compile(yaml: &str) -> HarnessPlan {
    compile_str(yaml).unwrap_or_else(|e| panic!("expected to compile: {e}"))
}

fn rejected(yaml: &str) -> String {
    compile_str(yaml)
        .expect_err("expected an error")
        .to_string()
}

type Steps = Vec<(String, Vec<String>)>;
type Env = Vec<(String, String)>;

fn gate(plan: &HarnessPlan, step: &str) -> (Steps, Env) {
    let PlanPrimitive::Gate { steps, env, .. } = &plan.steps[step].primitive else {
        panic!("{step} is a gate");
    };
    (
        steps
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    s.argv.iter().map(|a| String::from(a.clone())).collect(),
                )
            })
            .collect(),
        env.iter()
            .map(|(k, v)| (k.clone(), String::from(v.clone())))
            .collect(),
    )
}

#[test]
fn a_script_runs_under_bash_with_errexit_and_pipefail() {
    let plan = compile(DOC);
    let (steps, env) = gate(&plan, "work__v0_build_a0");
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].0, "script");
    assert_eq!(&steps[0].1[..4], ["bash", "-eo", "pipefail", "-c"]);
    assert!(
        steps[0].1[4].contains("make \"{{inputs.target}}\"\ntest -f out"),
        "the script is a template over inputs: {:?}",
        steps[0].1
    );
    assert_eq!(
        env,
        [
            ("PAD".to_string(), "{{facts.run.scratchpad}}".to_string()),
            ("TARGET".to_string(), "{{inputs.target}}".to_string())
        ]
    );
}

#[test]
fn a_list_of_named_steps_runs_in_order_under_sh() {
    let plan = compile(DOC);
    let (steps, _) = gate(&plan, "work__v1_staged_a0");
    let names: Vec<_> = steps.iter().map(|s| s.0.as_str()).collect();
    assert_eq!(names, ["compile", "test"]);
    assert_eq!(&steps[0].1[..3], ["sh", "-e", "-c"]);
    assert_eq!(steps[1].1[3], "./t");
}

#[test]
fn a_program_gate_is_one_step_and_may_carry_env_too() {
    let plan = compile(DOC);
    let (steps, env) = gate(&plan, "work__v2_plain_a0");
    assert_eq!(
        steps,
        [("checker".to_string(), vec!["checker".to_string()])]
    );
    assert_eq!(env, [("MODE".to_string(), "strict".to_string())]);
}

#[test]
fn the_failed_step_is_readable_after_and_in_a_repair_prompt() {
    let plan = compile(DOC);
    let json = serde_json::to_string(&plan).unwrap();
    assert!(
        json.contains("steps.work__v0_build_a0.failed_step"),
        "{json}"
    );
    assert!(json.contains("facts.gate_build.failed_step"), "{json}");
}

#[test]
fn a_gate_is_written_one_way_and_its_pieces_are_checked() {
    for (what, from, to, expect) in [
        (
            "both",
            "    timeout: 120\n",
            "    timeout: 120\n    command: [x]\n",
            "exactly one",
        ),
        (
            "neither",
            "    run: |\n      make \"{{ inputs.target }}\"\n      test -f out\n",
            "",
            "exactly one",
        ),
        (
            "shell",
            "  staged:\n    shell: sh",
            "  staged:\n    shell: zsh",
            "zsh",
        ),
        (
            "env name",
            "TARGET:",
            "\"bad-name\":",
            "environment variable",
        ),
        (
            "empty script",
            "{ name: test, script: \"./t\" }",
            "{ name: test, script: \" \" }",
            "script",
        ),
        ("step name", "name: test,", "name: compile,", "twice"),
        (
            "script input",
            "cc -o t {{ inputs.target }}.c",
            "cc -o t {{ inputs.nope }}.c",
            "not declared",
        ),
        (
            "env input",
            "TARGET: \"{{ inputs.target }}\"",
            "TARGET: \"{{ inputs.nope }}\"",
            "not declared",
        ),
        (
            "unknown key",
            "    timeout: 120\n",
            "    timeout: 120\n    scripts: x\n",
            "scripts",
        ),
    ] {
        let message = rejected(&DOC.replace(from, to));
        assert!(message.contains(expect), "{what}: {message}");
    }
}
