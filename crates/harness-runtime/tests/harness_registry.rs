// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness registry's spec: the named documents a session's children
//! may be. Registration compiles and validates the document offline, so a
//! name that resolves always names a complete, bounded plan; a run starts
//! from a registered plan without re-parsing it, entered at any step.

use sven_harness_runtime::{HarnessRegistry, HarnessRunData, HarnessRunError};

const RETURNER: &str = r#"
api_version: "1"
name: returner
entry: pick
steps:
  pick:
    branch: { on: "{{ inputs.first }}", when_true: done, when_false: branch }
  done:
    return: { value: { which: "entry" } }
  branch:
    return: { value: { which: "branch" } }
"#;

#[test]
fn a_registered_harness_resolves_by_its_own_name() {
    let registry = HarnessRegistry::new();
    let name = registry
        .register(RETURNER)
        .expect("valid harness registers");
    assert_eq!(name, "returner", "a plan registers under its own name");
    let plan = registry.resolve("returner").expect("the name resolves");
    assert_eq!(plan.name, "returner");
    assert!(registry.resolve("ghost").is_none());
}

#[test]
fn a_name_registers_once() {
    let registry = HarnessRegistry::new();
    let _ = registry.register(RETURNER).expect("first registration");
    let second = registry.register(RETURNER);
    let err = second.expect_err("the name is taken");
    assert!(err.to_string().contains("returner"), "{err}");
}

#[test]
fn an_invalid_document_is_rejected_at_registration() {
    let registry = HarnessRegistry::new();
    let err = registry
        .register("api_version: \"1\"\nname: broken\nsteps: {}")
        .expect_err("no entry step");
    assert!(matches!(err, HarnessRunError::Spec(_)), "{err}");
}

#[test]
fn a_run_starts_from_a_registered_plan_without_re_parsing() {
    let registry = HarnessRegistry::new();
    let _ = registry.register(RETURNER).expect("registers");
    let plan = registry.resolve("returner").expect("resolves");
    let run = HarnessRunData::from_parts((*plan).clone(), serde_json::json!({}));
    assert_eq!(run.plan().entry, "pick");
}

#[test]
fn a_run_can_enter_at_a_declared_branch() {
    let registry = HarnessRegistry::new();
    let _ = registry.register(RETURNER).expect("registers");
    let plan = registry.resolve("returner").expect("resolves");
    let run = HarnessRunData::from_parts((*plan).clone(), serde_json::json!({}))
        .with_entry("branch")
        .expect("branch is a step");
    assert_eq!(run.plan().entry, "branch");
    let err = HarnessRunData::from_parts((*plan).clone(), serde_json::json!({}))
        .with_entry("no_such_step");
    assert!(err.is_err(), "an unknown entry is rejected");
    let _ = registry;
}
