// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness can be understood without running it - validated offline,
//! inspected for its steps, capabilities, imports and digests, and reduced
//! to the input/output contract a caller must honour.
//!
//! Swedish Embedded AB implements offline verification for programmable agent
//! harnesses for its clients. If your team needs expertise in auditing what
//! an agent workflow may do before it runs then you can procure our services
//! by sending an email to info@swedishembedded.com.

mod common;

use common::*;
use serde_json::json;
use sven_harness_testkit::{inspect, io_schema, validate};

/// A workflow that hands its work to an imported one.
const PARENT: &str = r#"
api_version: "2"
name: parent
inputs: { subject: string }
imports:
  helper: ./lib/child.yaml
execution: { limits: { model_turns: 4 } }
workflow:
  - name: hand_over
    use: helper
    with: { subject: "{{ inputs.subject }}" }
outputs:
  out: { type: string, value: "{{ tasks.hand_over.output.kid }}" }
"#;

const CHILD: &str = r#"
api_version: "2"
name: child
inputs: { subject: string }
execution: { limits: { model_turns: 2 } }
workflow:
  - name: think
    model_only: true
    task: "Think about {{ inputs.subject }}"
outputs:
  kid: { type: string, value: "{{ tasks.think.output }}" }
"#;

#[test]
fn validate_accepts_a_package_with_its_imports() {
    let (tmp, _) = harness_dir(&[("harness.yaml", PARENT), ("lib/child.yaml", CHILD)]);
    let report = validate(tmp.path());
    assert!(report.valid, "{:?}", report.errors);
    let names: Vec<&str> = report.harnesses.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["parent"]);
    assert!(report.harnesses.iter().all(|h| !h.plan_digest.is_empty()));
    assert!(
        report.harnesses[0].bundle_digest.is_some(),
        "a package is pinned as one bundle"
    );
}

#[test]
fn validate_reports_the_problem_with_its_file() {
    let (tmp, _) = harness_dir(&[(
        "harness.yaml",
        "api_version: \"2\"\nname: broken\nworkflow: []\n",
    )]);
    let report = validate(tmp.path());
    assert!(!report.valid);
    assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
    assert!(report.errors[0].contains("harness.yaml"));
}

#[test]
fn validate_flags_an_import_that_is_not_there() {
    let (tmp, _) = harness_dir(&[("harness.yaml", PARENT)]);
    let report = validate(tmp.path());
    assert!(!report.valid);
    assert!(
        report.errors[0].contains("child.yaml"),
        "{:?}",
        report.errors
    );
}

#[test]
fn validate_of_a_missing_path_is_invalid_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let report = validate(&tmp.path().join("nowhere"));
    assert!(!report.valid);
}

#[test]
fn inspect_lists_steps_capabilities_cases_and_digests() {
    let (tmp, _) = harness_dir(&[
        ("harness.yaml", PARENT),
        ("lib/child.yaml", CHILD),
        ("cases/one.yaml", "x: 1"),
    ]);
    let report = inspect(tmp.path()).expect("inspects");
    assert_eq!(report.name, "parent");
    assert_eq!(report.entry, "hand_over__work");
    assert_eq!(report.capabilities, ["SpawnChild"]);
    assert!(!report.plan_digest.is_empty());
    assert!(
        report.bundle_digest.is_some(),
        "the imports are part of the program"
    );
    let step = report
        .steps
        .iter()
        .find(|s| s.id == "hand_over__work")
        .unwrap();
    assert_eq!(step.primitive, "delegate");
    assert_eq!(step.successors, ["complete"]);
    assert_eq!(report.cases, ["one"]);
    assert_eq!(
        report.limits.as_ref().and_then(|l| l.max_model_turns),
        Some(4)
    );
}

#[test]
fn inspect_is_deterministic() {
    let (tmp, _) = harness_dir(&[("harness.yaml", ROUTING)]);
    let a = serde_json::to_value(inspect(tmp.path()).unwrap()).unwrap();
    let b = serde_json::to_value(inspect(tmp.path()).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn io_schema_derives_the_contract_a_caller_must_honour() {
    let (tmp, _) = harness_dir(&[("harness.yaml", ROUTING)]);
    let schema = io_schema(tmp.path()).expect("a contract");
    assert_eq!(schema["name"], "model-routing");
    assert_eq!(
        schema["inputs"],
        json!({
            "type": "object",
            "properties": { "ticket": { "type": "string" } },
            "required": ["ticket"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        schema["outputs"]["properties"]["queue"],
        json!({"type": "string"})
    );
    assert_eq!(schema["outputs"]["required"], json!(["label", "queue"]));
}

#[test]
fn io_schema_maps_every_declared_type() {
    let doc = r#"
api_version: "2"
name: typed
inputs:
  s: string
  n: { type: number, description: "a count" }
  b: boolean
  j: json
execution: { limits: { model_turns: 1 } }
workflow:
  - name: only
    model_only: true
    task: Say ok.
"#;
    let (tmp, _) = harness_dir(&[("harness.yaml", doc)]);
    let props = io_schema(tmp.path()).unwrap()["inputs"]["properties"].clone();
    assert_eq!(props["s"], json!({"type": "string"}));
    assert_eq!(
        props["n"],
        json!({"type": "number", "description": "a count"})
    );
    assert_eq!(props["b"], json!({"type": "boolean"}));
    assert_eq!(props["j"], json!({}), "json accepts any value");
}
