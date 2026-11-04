// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a harness document can be checked without an engine: the SDK
//! compiles it exactly as loading it would, and says why when it cannot.
//!
//! Swedish Embedded AB implements validation of generated agent harnesses for
//! its clients. If your team needs expertise in checking machine-written
//! workflow documents then you can procure our services by sending an email
//! to info@swedishembedded.com.

use sven_sdk::{compile_harness, CallError};

const VALID: &str = r#"
api_version: "2"
name: ok
execution: { limits: { model_turns: 1 } }
workflow:
  - name: only
    model_only: true
    task: Say ok.
"#;

#[test]
fn a_valid_document_compiles_to_its_plan() {
    let plan = compile_harness(VALID).expect("compiles");
    assert_eq!(plan.name, "ok");
    assert_eq!(plan.digest.len(), 64);
}

#[test]
fn a_document_the_compiler_refuses_is_refused_with_the_compilers_reason() {
    let misspelled = VALID.replace("name: ok", "name: ok\nentrypoint: done");
    let Err(CallError::Precondition(why)) = compile_harness(&misspelled) else {
        panic!("a misspelled key must be refused");
    };
    assert!(
        why.contains("unknown top-level field `entrypoint`"),
        "{why}"
    );
}
