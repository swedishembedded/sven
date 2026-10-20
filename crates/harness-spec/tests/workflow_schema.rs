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

/// Every inline `r#"..."#` literal that is an `api_version: "2"` document the
/// compiler accepts, in the test sources that author them.
fn accepted_workflow_literals(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("r#\"") {
        let body = &rest[start + 3..];
        let Some(end) = body.find("\"#") else { break };
        let literal = &body[..end];
        if literal.contains("api_version: \"2\"") && sven_harness_spec::compile_str(literal).is_ok()
        {
            found.push(literal.to_string());
        }
        rest = &body[end + 2..];
    }
    found
}

#[test]
fn every_workflow_the_compiler_accepts_validates() {
    let validator = validator();
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0;
    for dir in [
        "tests",
        "../harness-runtime/tests",
        "../sdk/tests",
        "../harness-testkit/tests",
    ] {
        let Ok(entries) = std::fs::read_dir(manifest.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("dir entry").path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("readable");
            for document in accepted_workflow_literals(&source) {
                if let Err(error) = validator.validate(&yaml_to_json(&document)) {
                    panic!(
                        "{}: accepted by the compiler, rejected by the schema: {error}\n{document}",
                        path.display()
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 1,
        "expected inline workflow documents, saw {checked}"
    );

    // Shipped samples: every `workflow.yaml`.
    let samples = manifest.join("../../samples/harnesses");
    for entry in std::fs::read_dir(samples).expect("samples dir") {
        let file = entry.expect("dir entry").path().join("workflow.yaml");
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Err(error) = validator.validate(&yaml_to_json(&text)) {
                panic!(
                    "{}: the schema rejects this sample: {error}",
                    file.display()
                );
            }
        }
    }
}
