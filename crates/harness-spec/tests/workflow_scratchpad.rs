// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a run owns a private directory, the scratchpad. A workflow reads its
//! path as `{{ run.scratchpad }}` in prompts, system texts, gate arguments and
//! standard input, and may name where it lives with `execution.scratchpad`, a
//! template over the run's inputs only.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: scratch
inputs: { base: string }
execution:
  limits: { model_turns: 4 }
  scratchpad: "{{ inputs.base }}/work"
agent:
  system: "Notes go to {{ run.scratchpad }}/notes.md."
gates:
  judge:
    command: [check, "{{ run.scratchpad }}/report.json"]
    stdin: "{{ run.scratchpad }}"
workflow:
  - name: work
    task: "Write the report into {{ run.scratchpad }}."
    verify: [judge]
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
fn a_workflow_reads_the_scratchpad_in_its_texts_and_gates() {
    let plan = compile(DOC);
    let PlanPrimitive::Gate { steps, stdin, .. } = &plan.steps["work__v0_judge_a0"].primitive
    else {
        panic!("a gate step");
    };
    assert_eq!(
        steps[0].argv[1].paths()[0].as_text(),
        "facts.run.scratchpad"
    );
    assert_eq!(
        stdin.as_ref().expect("stdin").paths()[0].as_text(),
        "facts.run.scratchpad"
    );
    let json = serde_json::to_string(&plan).expect("plan json");
    assert!(
        json.matches("facts.run.scratchpad").count() >= 4,
        "the prompt, the system text, the argument and stdin read it: {json}"
    );
}

#[test]
fn the_document_names_the_scratchpad_as_a_template_over_inputs() {
    let plan = compile(DOC);
    assert_eq!(plan.scratchpad.as_deref(), Some("{{inputs.base}}/work"));
    let without = DOC.replace("  scratchpad: \"{{ inputs.base }}/work\"\n", "");
    assert_eq!(compile(&without).scratchpad, None);
    assert_ne!(
        plan.digest,
        compile(&without).digest,
        "where the scratchpad lives is part of the program"
    );
}

#[test]
fn the_scratchpad_template_reads_declared_inputs_and_nothing_else() {
    for (what, replacement, expect) in [
        ("an undeclared input", "{{ inputs.nope }}/w", "not declared"),
        ("itself", "{{ run.scratchpad }}/w", "inputs"),
        ("a task", "{{ tasks.work.output }}/w", "inputs"),
        ("an empty path", "  ", "scratchpad"),
    ] {
        let doc = DOC.replace("{{ inputs.base }}/work", replacement);
        let message = rejected(&doc);
        assert!(message.contains(expect), "{what}: {message}");
    }
}

#[test]
fn only_the_trajectory_file_and_the_scratchpad_are_run_values() {
    let message = rejected(&DOC.replace("run.scratchpad }}/notes", "run.workspace }}/notes"));
    assert!(message.contains("run.scratchpad"), "{message}");
}
