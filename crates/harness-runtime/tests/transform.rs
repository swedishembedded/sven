// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The transform step, driven through the machine's own dispatch: a
//! bundle's helper runs the step's input through the sandboxed interpreter
//! and the walk continues on the helper's output — or the run fails with
//! the code naming the boundary that broke.
//!
//! Helper fixtures are built with the wasm module encoder behind
//! `sven-harness-transform`'s `test-util` feature.

#![cfg(feature = "transform")]

mod common;

use std::collections::BTreeMap;

use base64::Engine as _;
use common::{drive, turn};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessState};
use sven_harness_spec::{HarnessBundle, Helper};
use sven_harness_transform::wasm::testutil as t;
use sven_harness_transform::wasm::ValType;
use sven_harness_transform::HELPER_ABI;
use sven_hsm::{Context, Event, Machine};

/// The echo helper: `run(ptr, len)` copies its input to the ABI's output
/// slot at 1024 and packs `(out_ptr << 32) | out_len` — output equals
/// input, byte for byte.
fn echo_module() -> Vec<u8> {
    let (b, run) = t::Builder::default().func(&[ValType::I32, ValType::I32], &[ValType::I64]);
    let mut ops = Vec::new();
    // Walk the input one byte at a time: leave when i >= len, else copy
    // mem[ptr + i] to mem[1024 + i] and step.
    ops.extend([t::BLOCK, t::EMPTY, t::LOOP, t::EMPTY]);
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(1));
    ops.extend([t::I32_GE_U]);
    ops.extend(t::br_if(1));
    // The destination goes below the value on the stack: a store pops its
    // value off the top, then the address it lands at.
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend(t::i32_const(1024));
    ops.extend([t::I32_ADD]);
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(0));
    ops.extend([t::I32_ADD, t::I32_LOAD8_U]);
    ops.extend(t::memarg(0, 0));
    ops.extend([t::I32_STORE8]);
    ops.extend(t::memarg(0, 0));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(2));
    ops.extend(t::i32_const(1));
    ops.extend([t::I32_ADD, t::LOCAL_SET]);
    ops.extend(t::idx(2));
    ops.extend(t::br(0));
    ops.extend([t::END, t::END]);
    // Pack the result: (out_ptr << 32) | out_len.
    ops.extend(t::i64_const(1024 << 32));
    ops.extend([t::LOCAL_GET]);
    ops.extend(t::idx(1));
    ops.extend([t::I64_EXTEND_I32_U, t::I64_OR]);
    b.body(&[(1, ValType::I32)], &ops)
        .memory(1)
        .export_func("run", run)
        .build()
}

/// A helper that runs forever: fuel exhaustion, one tick at a time.
fn runaway_module() -> Vec<u8> {
    let (b, run) = t::Builder::default().func(&[ValType::I32, ValType::I32], &[ValType::I64]);
    // The loop never exits, so the tail (making the function's i64 result
    // validate) is unreachable code the interpreter never reaches.
    b.body(&[], &[t::LOOP, t::EMPTY, t::BR, 0, t::END, 0x00])
        .memory(1)
        .export_func("run", run)
        .build()
}

/// A helper whose body traps immediately.
fn trapping_module() -> Vec<u8> {
    let (b, run) = t::Builder::default().func(&[ValType::I32, ValType::I32], &[ValType::I64]);
    b.body(&[], &[0x00]) // unreachable
        .memory(1)
        .export_func("run", run)
        .build()
}

fn helper(name: &str, module: &[u8]) -> (String, Helper) {
    (
        name.to_string(),
        Helper {
            abi: HELPER_ABI.to_string(),
            language: "wasm".to_string(),
            content: base64::engine::general_purpose::STANDARD.encode(module),
        },
    )
}

fn helper_abi(version: &str) -> Helper {
    Helper {
        abi: version.to_string(),
        language: "wasm".to_string(),
        content: String::new(),
    }
}

/// Compiles the document against `helpers` and starts a run, driven
/// through its initial transition.
fn start_bundle(
    yaml: &str,
    inputs: serde_json::Value,
    helpers: BTreeMap<String, Helper>,
) -> (HarnessMachine, Context, HarnessState) {
    let bundle = HarnessBundle {
        document: yaml.to_string(),
        helpers,
    };
    let run = HarnessRunData::from_bundle(&bundle, inputs).expect("valid bundle");
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state)
}

const ECHO: &str = r#"
api_version: "1"
name: shape
entry: prep
limits: { max_steps: 50 }
steps:
  prep:
    transform:
      helper: echo
      input:
        n: "{{ inputs.n }}"
      then: out
  out:
    return:
      value:
        got: "{{ steps.prep.value.n }}"
"#;

#[test]
fn a_transform_step_runs_its_helper_and_the_walk_carries_on() {
    let (m, _ctx, _state) = start_bundle(
        ECHO,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    let outcome = m.outcome().expect("the run returned");
    assert_eq!(
        outcome["got"], 41,
        "the helper's output is the stored value"
    );
}

#[test]
fn a_transform_step_can_store_as_a_fact() {
    let yaml = r#"
api_version: "1"
name: shape
entry: prep
limits: { max_steps: 50 }
steps:
  prep:
    transform:
      helper: echo
      input: { "n": "{{ inputs.n }}" }
      store_as: doubled
      then: out
  out:
    return:
      value: "{{ facts.doubled.n }}"
"#;
    let (m, _ctx, _state) = start_bundle(
        yaml,
        serde_json::json!({ "n": 7 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    assert_eq!(m.outcome().unwrap(), serde_json::json!(7));
}

#[test]
fn a_transform_step_without_its_helper_fails_the_run() {
    // A bare document compiles without helper checking; its run carries no
    // helpers, so the step fails when the walk reaches it.
    let run = HarnessRunData::new(ECHO, serde_json::json!({})).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    assert_eq!(m.outcome().unwrap()["code"], "helper_missing");
}

#[test]
fn a_helper_of_another_abi_is_refused_before_its_bytes_run() {
    let (m, _ctx, _state) = start_bundle(
        ECHO,
        serde_json::json!({}),
        BTreeMap::from([("echo".to_string(), helper_abi("helper-run/0"))]),
    );
    assert_eq!(m.outcome().unwrap()["code"], "helper_abi");
}

#[test]
fn a_helper_whose_fuel_runs_out_fails_the_run() {
    let yaml = ECHO.replace("max_steps: 50", "max_steps: 50, max_helper_fuel: 100");
    let (m, _ctx, _state) = start_bundle(
        &yaml,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &runaway_module())]),
    );
    assert_eq!(m.outcome().unwrap()["code"], "helper_fuel");
}

#[test]
fn a_trapping_helper_fails_the_run_with_its_trap_reported() {
    let (m, _ctx, _state) = start_bundle(
        ECHO,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &trapping_module())]),
    );
    let outcome = m.outcome().unwrap();
    assert_eq!(outcome["code"], "helper_trap");
    assert!(
        outcome["message"].as_str().unwrap().contains("unreachable"),
        "the trap is named, got {outcome}"
    );
}

#[test]
fn a_run_cannot_invent_helper_memory_it_was_not_given() {
    // The default page budget covers a normal helper; a helper whose input
    // does not fit fails at the memory boundary, not by shrinking the
    // budget under it.
    let big = serde_json::json!({ "n": "x".repeat(2_000_000) });
    let yaml = ECHO.replace("max_steps: 50", "max_steps: 50, max_helper_pages: 1");
    let (m, _ctx, _state) =
        start_bundle(&yaml, big, BTreeMap::from([helper("echo", &echo_module())]));
    assert_eq!(m.outcome().unwrap()["code"], "helper_memory");
}

#[test]
fn a_bundle_that_mismatches_its_helpers_never_starts() {
    // A `transform` step naming a helper the bundle does not carry is a
    // bundle compile error, refused offline.
    let bundle = HarnessBundle {
        document: ECHO.to_string(),
        helpers: BTreeMap::from([helper("other", &echo_module())]),
    };
    assert!(
        HarnessRunData::from_bundle(&bundle, serde_json::json!({})).is_err(),
        "a reference without a helper is a compile error"
    );
}

const PARK_ON_PREDICT: &str = r#"
api_version: "1"
name: shape
entry: prep
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  prep:
    transform:
      helper: echo
      input: { "n": "{{ inputs.n }}" }
      then: ask
  ask:
    predict:
      prompt: "Echo?"
      store_as: answer
      then: out
  out:
    return:
      value: { "n": "{{ steps.prep.value.n }}", "a": "{{ facts.answer }}" }
"#;

#[test]
fn a_checkpoint_pinned_to_a_bundle_resumes_under_it() {
    let (m, ctx, _state) = start_bundle(
        PARK_ON_PREDICT,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    assert!(
        matches!(m.step_effect(), Some(sven_hsm::Effect::CallLlm { .. })),
        "the walk parks on the predict step"
    );
    let snap = m.snapshot(&ctx);
    let bundle = HarnessBundle {
        document: PARK_ON_PREDICT.to_string(),
        helpers: BTreeMap::from([helper("echo", &echo_module())]),
    };
    let (mut m2, mut ctx2) =
        HarnessMachine::restore_bundle(&bundle, snap).expect("the same bundle resumes");
    assert_eq!(
        ctx2.fact(sven_harness_runtime::RUN_STATE_FACT)
            .and_then(|v| v.get("bundle_digest").and_then(|d| d.as_str()))
            .and_then(|d| if d.is_empty() {
                None
            } else {
                Some(d.to_string())
            }),
        Some(bundle.digest()),
        "the checkpoint pins the bundle digest"
    );
    let mut state = HarnessState::WaitingForModel;
    let _ = drive(&mut m2, &mut ctx2, &mut state, turn("harness.shape", "42"));
    assert_eq!(state, HarnessState::Completed);
    assert_eq!(
        m2.outcome().expect("finished"),
        serde_json::json!({ "n": 41, "a": "42" }),
        "the resumed run carries its helpers"
    );
}

#[test]
fn a_resume_under_a_different_bundle_is_refused() {
    let (m, ctx, _state) = start_bundle(
        PARK_ON_PREDICT,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    let _ = m.step_effect().expect("the walk parked");
    let snap = m.snapshot(&ctx);
    // The document is unchanged; only the helper bytes differ. Same plan,
    // different program.
    let changed = HarnessBundle {
        document: PARK_ON_PREDICT.to_string(),
        helpers: BTreeMap::from([helper("echo", &trapping_module())]),
    };
    let text = match HarnessMachine::restore_bundle(&changed, snap) {
        Err(sven_harness_runtime::HarnessRunError::PinnedBundleMismatch { .. }) => {
            "refused".to_string()
        }
        Err(e) => format!("wrong error: {e}"),
        Ok(_) => "accepted: a changed helper replaced a checkpointed one".to_string(),
    };
    assert_eq!(text, "refused");
}

#[test]
fn a_bundle_pinned_checkpoint_refuses_a_bare_document() {
    let (m, ctx, _state) = start_bundle(
        PARK_ON_PREDICT,
        serde_json::json!({ "n": 41 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    let _ = m.step_effect().expect("the walk parked");
    let snap = m.snapshot(&ctx);
    let text = match HarnessMachine::restore(PARK_ON_PREDICT.to_string(), snap) {
        Err(sven_harness_runtime::HarnessRunError::PinnedBundleMismatch { offered, .. }) => offered,
        Err(e) => format!("wrong error: {e}"),
        Ok(_) => "accepted: helpers vanished on resume".to_string(),
    };
    assert_eq!(text, "a bare document");
}

#[test]
fn a_registered_bundle_carries_its_helpers_to_its_children() {
    use sven_harness_runtime::HarnessRegistry;

    let registry = HarnessRegistry::new();
    let bundle = HarnessBundle {
        document: ECHO.to_string(),
        helpers: BTreeMap::from([helper("echo", &echo_module())]),
    };
    let name = registry.register_bundle(&bundle).expect("bundle registers");
    assert_eq!(name, "shape");
    assert!(registry.helpers("shape").contains_key("echo"));
    assert!(registry.helpers("nope").is_empty());
    assert!(
        registry.resolve("shape").is_some(),
        "the plan registers under its own name"
    );
}

#[test]
fn a_second_helper_run_gets_a_fresh_instance() {
    // Two transform steps in one run: state from the first invocation must
    // not leak into the second (the echo helper is stateless, but the
    // instance's memory starts fresh regardless — the first run's output
    // bytes at 1024 are gone when the second instance is built).
    let yaml = r#"
api_version: "1"
name: shape
entry: first
limits: { max_steps: 50 }
steps:
  first:
    transform:
      helper: echo
      input: { "n": "{{ inputs.a }}" }
      store_as: one
      then: second
  second:
    transform:
      helper: echo
      input: { "n": "{{ inputs.b }}" }
      then: out
  out:
    return:
      value: { "one": "{{ facts.one.n }}", "two": "{{ steps.second.value.n }}" }
"#;
    let (m, _ctx, _state) = start_bundle(
        yaml,
        serde_json::json!({ "a": 1, "b": 2 }),
        BTreeMap::from([helper("echo", &echo_module())]),
    );
    assert_eq!(
        m.outcome().unwrap(),
        serde_json::json!({ "one": 1, "two": 2 })
    );
}
