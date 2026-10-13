// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness can be understood without running it - validated offline,
//! inspected for its steps, capabilities, helpers and digests, and reduced
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

const PARENT: &str = r#"
api_version: "1"
name: parent
capabilities: [SpawnChild]
limits: { max_steps: 10 }
entry: hand_over
steps:
  hand_over: { delegate: { target: child, store_as: out, then: done } }
  done: { return: { value: "{{ steps.hand_over.value }}" } }
"#;

const CHILD: &str = r#"
api_version: "1"
name: child
limits: { max_steps: 10 }
entry: done
steps:
  done: { return: { value: "kid" } }
"#;

/// A bundle: the document text plus one helper whose bytes inspect never runs.
fn bundle_yaml() -> String {
    let document = r#"api_version: "1"
name: shaper
inputs: { raw: json }
outputs: { shaped: json }
limits: { max_steps: 10 }
entry: shape
steps:
  shape: { transform: { helper: reshape, input: "{{ inputs.raw }}", store_as: shaped, then: done } }
  done: { return: { value: { shaped: "{{ facts.shaped }}" } } }
"#;
    serde_yaml::to_string(&json!({
        "document": document,
        "helpers": { "reshape": { "abi": "helper-run/1", "language": "wasm", "content": "AGFzbQEAAAA=" } },
    }))
    .unwrap()
}

#[test]
fn validate_accepts_a_directory_with_its_children() {
    let (tmp, _) = harness_dir(&[("harness.yaml", PARENT), ("children/child.yaml", CHILD)]);
    let report = validate(tmp.path());
    assert!(report.valid, "{:?}", report.errors);
    let names: Vec<&str> = report.harnesses.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(
        names,
        ["child", "parent"],
        "children are compiled before the main document"
    );
    assert!(report.harnesses.iter().all(|h| !h.plan_digest.is_empty()));
}

#[test]
fn validate_reports_every_problem_with_its_file() {
    let (tmp, _) = harness_dir(&[
        (
            "harness.yaml",
            "api_version: \"1\"\nname: broken\nentry: nowhere\nsteps: {}\n",
        ),
        ("children/child.yaml", "not: [valid"),
    ]);
    let report = validate(tmp.path());
    assert!(!report.valid);
    assert_eq!(report.errors.len(), 2, "{:?}", report.errors);
    assert!(report.errors.iter().any(|e| e.contains("child.yaml")));
    assert!(report.errors.iter().any(|e| e.contains("harness.yaml")));
}

#[test]
fn validate_flags_a_delegate_to_a_harness_that_is_not_there() {
    let (tmp, _) = harness_dir(&[("harness.yaml", PARENT)]);
    let report = validate(tmp.path());
    assert!(!report.valid);
    assert!(report.errors[0].contains("child"), "{:?}", report.errors);
}

#[test]
fn validate_of_a_missing_path_is_invalid_not_a_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let report = validate(&tmp.path().join("nowhere"));
    assert!(!report.valid);
}

#[test]
fn inspect_lists_steps_capabilities_children_cases_and_digest() {
    let (tmp, _) = harness_dir(&[
        ("harness.yaml", PARENT),
        ("children/child.yaml", CHILD),
        ("cases/one.yaml", "x: 1"),
    ]);
    let report = inspect(tmp.path()).expect("inspects");
    assert_eq!(report.name, "parent");
    assert_eq!(report.entry, "hand_over");
    assert_eq!(report.capabilities, ["SpawnChild"]);
    assert!(!report.plan_digest.is_empty());
    assert!(report.bundle_digest.is_none());
    let step = report.steps.iter().find(|s| s.id == "hand_over").unwrap();
    assert_eq!(step.primitive, "delegate");
    assert_eq!(step.successors, ["done"]);
    assert_eq!(report.children.len(), 1);
    assert_eq!(report.children[0].name, "child");
    assert_eq!(report.cases, ["one"]);
    assert_eq!(report.limits.as_ref().and_then(|l| l.max_steps), Some(10));
}

#[test]
fn inspect_names_a_bundles_helpers_and_pins_its_digest() {
    let (tmp, _) = harness_dir(&[("bundle.yaml", &bundle_yaml())]);
    let report = inspect(tmp.path()).expect("a bundle inspects");
    assert_eq!(report.name, "shaper");
    assert!(report.bundle_digest.is_some());
    assert_eq!(report.helpers.len(), 1);
    assert_eq!(report.helpers[0].name, "reshape");
    assert_eq!(report.helpers[0].abi, "helper-run/1");
    assert_eq!(report.helpers[0].language, "wasm");
    assert_eq!(report.helpers[0].bytes, 8);

    let report = validate(tmp.path());
    assert!(report.valid, "{:?}", report.errors);
    assert!(report.harnesses[0].bundle_digest.is_some());
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
api_version: "1"
name: typed
inputs:
  s: string
  n: { type: number, description: "a count" }
  b: boolean
  j: json
limits: { max_steps: 5 }
entry: done
steps:
  done: { return: { value: "ok" } }
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
