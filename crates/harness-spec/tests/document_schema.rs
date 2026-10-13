// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The published JSON Schema for harness documents is held to the compiler.
//!
//! The schema is what editors and `sven harness schema` hand to authors, so
//! it must name exactly the primitives the compiler knows and accept every
//! document the compiler accepts. These tests pin both directions.
//!
//! Swedish Embedded AB implements editor-checkable, data-defined agent
//! harnesses for its clients. If your team needs expertise in declarative
//! agent workflow languages then you can procure our services by sending an
//! email to info@swedishembedded.com.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sven_harness_spec::{HarnessDocument, DOCUMENT_SCHEMA, PRIMITIVES};

fn schema_json() -> serde_json::Value {
    serde_json::from_str(DOCUMENT_SCHEMA).expect("the document schema is valid JSON")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&schema_json()).expect("the document schema is a valid JSON Schema")
}

fn yaml_to_json(yaml: &str) -> serde_json::Value {
    serde_yaml::from_str(yaml).expect("document is YAML")
}

/// A document using each of the twelve primitives once.
const ALL_PRIMITIVES: &str = r#"
api_version: "1"
name: everything
description: "one of each primitive"
inputs:
  topic: string
  count: { type: number, description: "how many" }
outputs: { out: json }
capabilities: [ReadFile, SpawnChild, RunVerifier]
limits:
  max_steps: 50
  max_model_turns: 4
  max_tool_calls: 4
  max_delegate_depth: 3
  max_parallel: 2
  max_helper_fuel: 1000
  max_helper_pages: 4
schemas:
  answer: { type: object, properties: { ok: { type: boolean } } }
entry: ask
steps:
  ask:      { predict: { prompt: "q {{ inputs.topic }}", schema: answer, store_as: a, then: work } }
  work:     { agent: { prompt: "do it", tools: [read_file], store_as: w, then: act } }
  act:      { tool: { name: read_file, args: { path: "x" }, store_as: t, then: check } }
  check:    { verify: { spec: { file_exists: { path: "x" } }, store_as: v, then: set } }
  set:      { assign: { set: { n: 1, s: "{{ facts.v }}" }, then: pick } }
  pick:     { branch: { on: { all: ["facts.n", { not: "!facts.v" }, { eq: "facts.s", value: "passed" }], }, when_true: again, when_false: fan } }
  again:    { retry: { attempts: 2, body: set, then: fan, on_exhausted: bad } }
  fan:      { parallel: { branches: [x, y], join: ask_child } }
  x:        { return: { value: 1 } }
  y:        { return: { value: 2 } }
  ask_child: { delegate: { target: child, as_kind: harness, input: "{{ inputs.topic }}", store_as: c, then: shape } }
  shape:    { transform: { helper: reshape, input: "{{ facts.c }}", store_as: r, then: done } }
  done:     { return: { value: { out: "{{ facts.r }}" } } }
  bad:      { fail: { code: nope, message: "gave up" } }
"#;

/// Every `r#"..."#` literal in a Rust source file that parses as a harness
/// document that the compiler accepts (negative-test documents that the
/// compiler rejects are deliberately left out).
fn parsed_documents_in(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("r#\"") {
        let body = &rest[start + 3..];
        let Some(end) = body.find("\"#") else { break };
        let literal = &body[..end];
        if literal.contains("api_version")
            && HarnessDocument::parse(literal)
                .and_then(|d| d.compile())
                .is_ok()
        {
            found.push(literal.to_string());
        }
        rest = &body[end + 2..];
    }
    found
}

fn rust_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    out.sort();
    out
}

fn yaml_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
        .collect();
    out.sort();
    out
}

#[test]
fn the_schema_names_exactly_the_twelve_primitives() {
    let schema = schema_json();
    let branches = schema["$defs"]["step"]["oneOf"]
        .as_array()
        .expect("$defs.step.oneOf is an array");
    let named: BTreeSet<String> = branches
        .iter()
        .map(|branch| {
            let required = branch["required"].as_array().expect("required list");
            assert_eq!(required.len(), 1, "a step holds exactly one primitive key");
            required[0].as_str().expect("key").to_string()
        })
        .collect();
    let constant: BTreeSet<String> = PRIMITIVES.iter().map(|p| (*p).to_string()).collect();
    assert_eq!(PRIMITIVES.len(), 12);
    assert_eq!(named, constant, "schema and PRIMITIVES must agree");

    // The compiler's own view: the keys of a document that uses each one.
    let document = HarnessDocument::parse(ALL_PRIMITIVES).expect("all-primitives document parses");
    let compiled: BTreeSet<String> = document
        .spec()
        .steps
        .values()
        .map(|step| step.key().to_string())
        .collect();
    assert_eq!(
        compiled, constant,
        "StepSpec::key must agree with PRIMITIVES"
    );

    assert!(
        validator().is_valid(&yaml_to_json(ALL_PRIMITIVES)),
        "the all-primitives document must validate"
    );
}

#[test]
fn every_test_and_sample_document_validates() {
    let validator = validator();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut checked = 0usize;

    // Documents written inline in the language's own tests and in the
    // runtime and SDK suites that execute them.
    for dir in ["tests", "../harness-runtime/tests", "../sdk/tests"] {
        for file in rust_files_in(&manifest.join(dir)) {
            let source = std::fs::read_to_string(&file).expect("test source is readable");
            for document in parsed_documents_in(&source) {
                let instance = yaml_to_json(&document);
                if let Err(error) = validator.validate(&instance) {
                    panic!(
                        "{}: the compiler accepts this document but the schema rejects it: {error}\n{document}",
                        file.display()
                    );
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked >= 20,
        "expected many inline documents, saw {checked}"
    );

    // Sample harness directories: the main document and its children. Zero
    // samples exist until the samples land; a sample that exists must pass.
    let samples = manifest.join("../../samples/harnesses");
    if let Ok(entries) = std::fs::read_dir(&samples) {
        for entry in entries {
            let dir = entry.expect("dir entry").path();
            let main = dir.join("harness.yaml");
            let mut files = yaml_files_in(&dir.join("children"));
            if main.is_file() {
                files.push(main);
            }
            for file in files {
                let text = std::fs::read_to_string(&file).expect("sample is readable");
                let instance = yaml_to_json(&text);
                if let Err(error) = validator.validate(&instance) {
                    panic!(
                        "{}: the schema rejects this sample: {error}",
                        file.display()
                    );
                }
            }
        }
    }
}

#[test]
fn an_unknown_primitive_fails_the_schema() {
    let instance = yaml_to_json(
        r#"
api_version: "1"
name: unknown
entry: bad
steps:
  bad:
    teleport: {}
"#,
    );
    assert!(!validator().is_valid(&instance));
}

#[test]
fn a_step_with_two_primitives_fails_the_schema() {
    let instance = yaml_to_json(
        r#"
api_version: "1"
name: two
entry: bad
steps:
  bad:
    fail: { code: a }
    return: { value: 1 }
"#,
    );
    assert!(!validator().is_valid(&instance));
}

#[test]
fn the_schema_rejects_what_the_typed_parser_rejects() {
    let validator = validator();
    let cases = [
        ("a missing entry", "api_version: \"1\"\nname: n\nsteps: { a: { fail: {} } }\n"),
        (
            "a retry without a body",
            "api_version: \"1\"\nname: n\nentry: r\nsteps:\n  r: { retry: { attempts: 2, then: d } }\n",
        ),
        (
            "an unknown capability",
            "api_version: \"1\"\nname: n\nentry: a\ncapabilities: [Teleport]\nsteps: { a: { fail: {} } }\n",
        ),
        (
            "an unknown input type",
            "api_version: \"1\"\nname: n\nentry: a\ninputs: { x: float }\nsteps: { a: { fail: {} } }\n",
        ),
        (
            "an unknown branch form",
            "api_version: \"1\"\nname: n\nentry: a\nsteps:\n  a: { branch: { on: { teleport: 1 }, when_true: a, when_false: a } }\n",
        ),
    ];
    for (what, yaml) in cases {
        assert!(
            !validator.is_valid(&yaml_to_json(yaml)),
            "{what} must fail the schema"
        );
    }
}
