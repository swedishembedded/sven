// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: four gaps a port found. A skipped task leaves an explicit empty output
//! and the stop `skipped` behind; repair attempts and follow-up repeats may come
//! from a number input, up to a bound the plan is unrolled to; a gate may keep
//! more of its output than 16 KiB.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in turning long-running agent
//! work into reviewable, resumable programs then you can procure our services
//! by sending an email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, HarnessPlan, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: gaps
inputs: { tries: number, more: number, pick: boolean }
execution: { limits: { model_turns: 40 } }
gates:
  check: { command: [sh, check.sh], output_bytes: 65536 }
workflow:
  - name: optional
    when: inputs.pick
    task: Maybe.
  - name: work
    task: Do it. {{ tasks.optional.output }}
    verify: [check]
    repair: { attempts: "{{ inputs.tries }}", at_most: 3 }
    follow_ups:
      - task: Again.
        max: "{{ inputs.more }}"
        at_most: 2
outputs:
  said: { type: string, value: "[{{ tasks.optional.output }}] {{ tasks.optional.stop }}" }
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
fn a_skipped_task_leaves_an_empty_output_and_the_stop_skipped() {
    let plan = compile(DOC);
    let PlanPrimitive::Assign { set, then } = &plan.steps["optional__skip"].primitive else {
        panic!("an assign step");
    };
    assert_eq!(set["task_optional"], serde_json::json!(""));
    assert_eq!(set["stop_optional"], serde_json::json!("skipped"));
    assert_eq!(then, "work__work", "the next task runs after the skip");
}

#[test]
fn repair_and_follow_up_counts_can_come_from_an_input() {
    let plan = compile(DOC);
    // Three repair attempts are unrolled, the second and third behind guards.
    let repairs = plan
        .steps
        .keys()
        .filter(|id| id.ends_with("_repair"))
        .count();
    let guards = plan
        .steps
        .keys()
        .filter(|id| id.ends_with("_repair_if"))
        .count();
    assert_eq!(repairs, 3);
    assert_eq!(guards, 3, "each unrolled attempt asks the input first");
    let sends = plan
        .steps
        .keys()
        .filter(|id| id.starts_with("work__f0"))
        .count();
    assert!(sends >= 4, "two sends, each with its guard: {sends}");

    for (what, yaml, why) in [
        (
            "an input without at_most",
            DOC.replace("at_most: 3", "").replace(
                "{ attempts: \"{{ inputs.tries }}\", }",
                "{ attempts: \"{{ inputs.tries }}\" }",
            ),
            "at_most",
        ),
        (
            "an undeclared input",
            DOC.replace("inputs.tries", "inputs.nope"),
            "declared",
        ),
        (
            "a text input",
            DOC.replace("tries: number", "tries: string"),
            "type number",
        ),
        (
            "at_most beside a literal",
            DOC.replace("attempts: \"{{ inputs.tries }}\"", "attempts: 2"),
            "at_most",
        ),
        (
            "too large a bound",
            DOC.replace("at_most: 3", "at_most: 99"),
            "at_most",
        ),
        (
            "a follow-up bound too large",
            DOC.replace("at_most: 2", "at_most: 9"),
            "at_most",
        ),
    ] {
        assert!(rejected(&yaml).contains(why), "{what}: {}", rejected(&yaml));
    }
}

#[test]
fn a_gate_may_keep_more_of_its_output_within_a_limit() {
    let plan = compile(DOC);
    let kept = plan.steps.values().find_map(|s| match &s.primitive {
        PlanPrimitive::Gate { output_bytes, .. } => Some(*output_bytes),
        _ => None,
    });
    assert_eq!(kept, Some(Some(65536)));
    assert!(rejected(&DOC.replace("65536", "100")).contains("output_bytes"));
    assert!(rejected(&DOC.replace("65536", "5000000")).contains("output_bytes"));
}

const VARIANTS: &str = r#"
api_version: "2"
name: variants
inputs: { mode: string }
execution: { limits: { model_turns: 4 } }
agent:
  system:
    - when: { eq: inputs.mode, value: strict }
      text: Strict.
    - text: Neutral.
workflow:
  - name: work
    task: Do it.
"#;

#[test]
fn agent_system_variants_compile_and_an_unconditional_last_entry_is_required() {
    compile(VARIANTS);

    let open = VARIANTS.replace(
        "    - text: Neutral.\n",
        "    - when: { eq: inputs.mode, value: lax }\n      text: Lax.\n",
    );
    assert!(
        rejected(&open).contains("last `agent.system` entry"),
        "{}",
        rejected(&open)
    );

    let shadowed = VARIANTS.replace(
        "    - when: { eq: inputs.mode, value: strict }\n      text: Strict.\n    - text: Neutral.\n",
        "    - text: Neutral.\n    - text: Strict.\n",
    );
    assert!(
        rejected(&shadowed).contains("unreachable"),
        "{}",
        rejected(&shadowed)
    );

    let reads_a_task = VARIANTS.replace("inputs.mode", "tasks.work.stop");
    assert!(
        rejected(&reads_a_task).contains("agent.system"),
        "{}",
        rejected(&reads_a_task)
    );
}
