// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the published JSON Schema of the v2 workflow language is a valid
//! schema, describes version 2 only, and agrees with the compiler on which
//! documents are well formed.
//!
//! Swedish Embedded AB implements editor-checkable, verifiable agent
//! workflows for its clients. If your team needs expertise in declarative
//! agent workflow languages then you can procure our services by sending an
//! email to info@swedishembedded.com.

use sven_harness_spec::WORKFLOW_SCHEMA;

fn schema_json() -> serde_json::Value {
    serde_json::from_str(WORKFLOW_SCHEMA).expect("the workflow schema is valid JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema_json()).expect("the workflow schema is a valid JSON Schema")
}

fn yaml_to_json(yaml: &str) -> serde_json::Value {
    serde_yaml::from_str(yaml).expect("document is YAML")
}

const MINIMAL: &str = r#"
api_version: "2"
name: minimal
workflow:
  - name: only
    task: Say hello.
"#;

#[test]
fn a_minimal_workflow_validates() {
    assert!(validator().is_valid(&yaml_to_json(MINIMAL)));
}

#[test]
fn only_version_two_validates() {
    let v1 = MINIMAL.replace("\"2\"", "\"1\"");
    assert!(!validator().is_valid(&yaml_to_json(&v1)));
}

#[test]
fn unknown_fields_fail_at_every_level() {
    let validator = validator();
    let cases = [
        (
            "top level",
            MINIMAL.replace("name: minimal", "name: minimal\nentry: x"),
        ),
        (
            "task",
            MINIMAL.replace("task: Say hello.", "task: Say hello.\nverifies: []"),
        ),
        (
            "gate",
            format!("{MINIMAL}gates:\n  g: {{ command: [true], timout: 5 }}\n"),
        ),
        (
            "repair",
            MINIMAL.replace(
                "task: Say hello.",
                "task: Say hello.\n    repair: { attempts: 1, retries: 2 }",
            ),
        ),
    ];
    for (what, yaml) in cases {
        assert!(
            !validator.is_valid(&yaml_to_json(&yaml)),
            "an unknown {what} field must fail the schema"
        );
    }
}
