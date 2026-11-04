// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: a repair turn can have a tool-call allowance of its own, so a task
//! whose first turn is bounded (its checks run every N calls) still gives
//! each repair room to work, instead of drawing on what the first turn left.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in agent workflows that check
//! their work as they go then you can procure our services by sending an
//! email to info@swedishembedded.com.

use sven_harness_spec::{compile_str, PlanPrimitive};

const DOC: &str = r#"
api_version: "2"
name: bounded
execution: { limits: { model_turns: 40 } }
gates:
  ok: { command: ["true"] }
workflow:
  - name: design
    task: Build it.
    allowance: 60
    verify: [ok]
    repair: { attempts: 2, prompt: Fix it., allowance: 60 }
"#;

#[test]
fn every_repair_turn_starts_with_its_own_allowance() {
    let plan = compile_str(DOC).expect("compiles");
    let repairs: Vec<_> = plan
        .steps
        .iter()
        .filter(|(id, _)| id.contains("repair"))
        .filter_map(|(id, step)| match &step.primitive {
            PlanPrimitive::Agent { allowance, .. } => Some((id.clone(), allowance.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(repairs.len(), 2, "{repairs:?}");
    for (id, allowance) in repairs {
        let allowance = allowance.map(String::from);
        assert_eq!(allowance.as_deref(), Some("60"), "`{id}`");
    }
}

#[test]
fn a_repair_allowance_is_a_whole_number_on_a_turn_with_tools() {
    let bad = DOC.replace("allowance: 60 }", "allowance: lots }");
    let error = compile_str(&bad).unwrap_err().to_string();
    assert!(error.contains("allowance"), "{error}");
    let model_only = DOC.replace("    allowance: 60\n", "    model_only: true\n");
    let error = compile_str(&model_only).unwrap_err().to_string();
    assert!(error.contains("makes no tool calls"), "{error}");
}
