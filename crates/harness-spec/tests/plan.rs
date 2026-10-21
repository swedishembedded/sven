// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec-level acceptance tests for the validator and the plan compiler, over
//! plans written directly in the step language (the `testing` feature): what
//! the validator rejects before a plan exists, and that a plan's digest is a
//! pure function of what it means.

use sven_harness_spec::testing::compile_steps as doc;

/// The smallest complete harness: one entry step that returns.
const MINIMAL: &str = r#"
api_version: "2"
name: minimal
entry: done
outputs:
  result: string
steps:
  done:
    return:
      value:
        result: "{{ inputs.topic }}"
"#;

#[test]
fn a_minimal_document_parses_and_compiles() {
    let plan = doc(MINIMAL).expect("minimal must compile");
    assert_eq!(plan.name, "minimal");
    assert_eq!(plan.entry, "done");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.digest.len(), 64);
}

#[test]
fn the_digest_is_invariant_to_yaml_key_order_and_whitespace() {
    let reordered = r#"
steps:
  done:
    return:
      value:
        result: "{{ inputs.topic }}"
outputs:
  result: string
name: minimal
entry: done
api_version: "2"
"#;
    let a = doc(MINIMAL).expect("a");
    let b = doc(reordered).expect("b");
    assert_eq!(a.digest, b.digest, "key order must not change the digest");
    assert_eq!(a.steps, b.steps);
}

#[test]
fn the_digest_changes_when_the_program_meaning_changes() {
    let changed = MINIMAL.replace("inputs.topic", "inputs.subject");
    let a = doc(MINIMAL).expect("a");
    let b = doc(&changed).expect("b");
    assert_ne!(a.digest, b.digest);
}

#[test]
fn an_unsupported_api_version_is_rejected() {
    let yaml = MINIMAL.replace("api_version: \"2\"", "api_version: \"99\"");
    let err = doc(&yaml).expect_err("unsupported version must be rejected");
    assert!(err.to_string().contains("api_version"), "{err}");
}

#[test]
fn a_missing_entry_step_is_rejected() {
    let yaml = MINIMAL.replace("entry: done", "entry: nowhere");
    let err = doc(&yaml).expect_err("missing entry must be rejected");
    assert!(err.to_string().contains("nowhere"), "{err}");
}

#[test]
fn an_unbounded_cycle_is_rejected() {
    let yaml = r#"
api_version: "2"
name: unbounded
entry: loop
steps:
  loop:
    branch:
      on: "{{ facts.go }}"
      when_true: loop
      when_false: loop
"#;
    let err = doc(yaml).expect_err("unbounded cycle must be rejected");
    assert!(err.to_string().contains("loop"), "{err}");
}

#[test]
fn a_dangling_branch_target_is_rejected() {
    let yaml = r#"
api_version: "2"
name: dangling
entry: pick
steps:
  pick:
    branch:
      on: "{{ inputs.ok }}"
      when_true: nowhere
      when_false: done
  done:
    return:
      value: {}
"#;
    let err = doc(yaml).expect_err("dangling target must be rejected");
    assert!(err.to_string().contains("nowhere"), "{err}");
}

#[test]
fn an_unknown_primitive_is_rejected_with_the_step_named() {
    let yaml = r#"
api_version: "2"
name: unknown
entry: bad
steps:
  bad:
    teleport: {}
"#;
    let err = doc(yaml).expect_err("unknown primitive must be rejected");
    assert!(err.to_string().contains("teleport"), "{err}");
}

#[test]
fn a_missing_limits_block_is_rejected_when_steps_can_run_away() {
    // A harness with model/tool/verify primitives must declare limits.
    let yaml = r#"
api_version: "2"
name: nolimits
entry: think
steps:
  think:
    predict:
      prompt: "hello"
      then: done
  done:
    return:
      value: {}
"#;
    let err = doc(yaml).expect_err("missing limits must be rejected");
    assert!(err.to_string().contains("limits"), "{err}");
}

#[test]
fn returning_a_value_that_does_not_match_outputs_is_rejected() {
    let yaml = r#"
api_version: "2"
name: mismatch
entry: done
outputs:
  result: string
steps:
  done:
    return:
      value:
        wrong: 1
"#;
    let err = doc(yaml).expect_err("return must match the output contract");
    assert!(err.to_string().contains("result"), "{err}");
}

#[test]
fn inputs_are_typed_and_referenced_by_name() {
    let yaml = r#"
api_version: "2"
name: typed
inputs:
  topic: string
entry: done
outputs:
  result: string
steps:
  done:
    return:
      value:
        result: "{{ inputs.topic }}"
"#;
    let plan = doc(yaml).expect("typed inputs compile");
    assert!(plan
        .inputs
        .as_ref()
        .is_some_and(|inputs| inputs.contains_key("topic")));
}

#[test]
fn a_predict_step_names_a_schema_declared_in_the_document() {
    let yaml = r#"
api_version: "2"
name: shaped
entry: ask
limits: { max_steps: 10, max_model_turns: 3 }
schemas:
  answer:
    type: object
    properties:
      greeting: { type: string }
    required: [greeting]
steps:
  ask:
    predict:
      prompt: "Say hello"
      schema: answer
      store_as: spoken
      then: done
  done:
    return:
      value: {}
"#;
    let plan = doc(yaml).expect("a named schema must compile");
    let Some(schema) = plan.schemas.as_ref().and_then(|s| s.get("answer")) else {
        panic!("the declared schema must be carried in the plan");
    };
    assert_eq!(schema["type"], "object");
    let sven_harness_spec::PlanPrimitive::Predict { schema: named, .. } =
        &plan.step("ask").expect("step").primitive
    else {
        panic!("ask must be a predict step");
    };
    assert_eq!(named.as_deref(), Some("answer"));
}

#[test]
fn a_schema_name_without_a_declared_schema_is_rejected() {
    let yaml = r#"
api_version: "2"
name: dangling
entry: ask
limits: { max_steps: 10, max_model_turns: 3 }
steps:
  ask:
    predict:
      prompt: "Say hello"
      schema: nope
      then: done
  done:
    return:
      value: {}
"#;
    let err = doc(yaml).expect_err("a dangling schema name must be rejected");
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn a_declared_but_unused_schema_is_rejected() {
    let yaml = r#"
api_version: "2"
name: unused
entry: done
schemas:
  orphan:
    type: string
steps:
  done:
    return:
      value: {}
"#;
    let err = doc(yaml).expect_err("an unreferenced schema must be rejected");
    assert!(err.to_string().contains("orphan"), "{err}");
}

#[test]
fn a_schema_that_is_not_an_object_is_rejected() {
    let yaml = r#"
api_version: "2"
name: badshape
entry: ask
limits: { max_steps: 10, max_model_turns: 3 }
schemas:
  answer: "not a schema"
steps:
  ask:
    predict:
      prompt: "Say hello"
      schema: answer
      then: done
  done:
    return:
      value: {}
"#;
    let err = doc(yaml).expect_err("a non-object schema must be rejected");
    assert!(err.to_string().contains("answer"), "{err}");
}

#[test]
fn a_declared_schema_changes_the_plan_digest() {
    let base = r#"
api_version: "2"
name: pinned
entry: ask
limits: { max_steps: 10, max_model_turns: 3 }
schemas:
  answer: { type: string }
steps:
  ask:
    predict:
      prompt: "Say hello"
      schema: answer
      then: done
  done:
    return:
      value: {}
"#;
    let changed = base.replace("type: string", "type: integer");
    let a = doc(base).expect("a");
    let b = doc(&changed).expect("b");
    assert_ne!(a.digest, b.digest, "schema bytes are program meaning");
}

#[test]
fn an_unknown_top_level_key_is_rejected_by_name() {
    let yaml = MINIMAL.replace("name: minimal", "name: minimal\nentrypoint: done");
    let err = doc(&yaml).expect_err("a misspelled top-level key must not be ignored");
    let text = err.to_string();
    assert!(text.contains("unknown field `entrypoint`"), "{text}");
}

#[test]
fn an_unknown_limit_is_rejected() {
    let yaml = MINIMAL.replace("entry: done", "entry: done\nlimits:\n  max_stepz: 3");
    let text = doc(&yaml).expect_err("misspelled limit").to_string();
    assert!(text.contains("unknown field `max_stepz`"), "{text}");
}

#[test]
fn an_unknown_field_in_a_declared_io_value_is_rejected() {
    let yaml = MINIMAL.replace("result: string", "result: { type: string, descripton: x }");
    assert!(doc(&yaml).is_err(), "misspelled io value field");
}

#[test]
fn a_step_no_path_from_entry_reaches_is_rejected_by_name() {
    let yaml = r#"
api_version: "2"
name: orphan
entry: done
steps:
  done:
    return: { value: 1 }
  stray:
    return: { value: 2 }
"#;
    let text = doc(yaml)
        .expect_err("an orphan step is dead code")
        .to_string();
    assert!(
        text.contains("stray") && text.contains("unreachable"),
        "{text}"
    );
}

#[test]
fn a_gate_needs_the_shell_capability_declared() {
    let body = r#"
api_version: "2"
name: gated
entry: act
limits: { max_steps: 4 }
steps:
  act:
    gate:
      steps: [{ name: ls, argv: [ls] }]
      then: done
      on_fail: bad
  done:
    return: { value: {} }
  bad:
    fail: { code: no }
"#;
    let err = doc(body).expect_err("an undeclared capability must be rejected");
    assert!(err.to_string().contains("ExecuteShell"), "{err}");

    let declared = body.replace("entry: act", "entry: act\ncapabilities: [ExecuteShell]");
    let plan = doc(&declared).expect("declaring the capability admits the gate");
    assert_eq!(plan.capabilities, vec!["ExecuteShell"]);
}

#[test]
fn a_delegate_needs_spawn_child_declared() {
    let body = r#"
api_version: "2"
name: parent
entry: ask
limits: { max_steps: 4 }
steps:
  ask:
    delegate: { target: child, input: {}, then: done }
  done:
    return: { value: {} }
"#;
    let err = doc(body).expect_err("a child needs the capability");
    assert!(err.to_string().contains("SpawnChild"), "{err}");
    doc(&body.replace("entry: ask", "entry: ask\ncapabilities: [SpawnChild]"))
        .expect("declaring SpawnChild admits the delegate");
}

#[test]
fn an_unknown_field_inside_a_step_names_the_primitive_and_the_field() {
    let yaml = r#"
api_version: "2"
name: typo
entry: act
steps:
  act:
    assign:
      sets: { a: 1 }
      then: done
  done:
    return: { value: 1 }
"#;
    let text = doc(yaml)
        .expect_err("`sets` is not an assign field")
        .to_string();
    assert!(text.contains("`assign`"), "{text}");
    assert!(text.contains("unknown field `sets`"), "{text}");
}
