// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness checkpoint's spec: a snapshot resumes at the step it stopped
//! on, and only under the plan it was taken against — a checkpoint records
//! the digest of the plan it started under, and a modified document cannot
//! silently replace the checkpointed version.

mod common;

use common::{drive, start, turn};
use sven_harness_runtime::{HarnessMachine, RUN_STATE_FACT};
use sven_hsm::{Effect, Event};

const GREET: &str = r#"
api_version: "1"
name: greet
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Say hello to {{ inputs.name }}"
      store_as: greeting
      then: save
  save:
    assign:
      set:
        spoken: "{{ steps.ask.value }}"
      then: out
  out:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

/// The same harness, edited after the fact: the plan a checkpoint recorded
/// is no longer the plan this document compiles to.
const GREET_EDITED: &str = r#"
api_version: "1"
name: greet
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Say goodbye to {{ inputs.name }}"
      store_as: greeting
      then: save
  save:
    assign:
      set:
        spoken: "never {{ steps.ask.value }}"
      then: out
  out:
    return:
      value:
        message: "{{ facts.spoken }}"
"#;

#[test]
fn a_started_run_records_the_digest_of_the_plan_it_started_under() {
    let (m, ctx, _state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let digest = ctx
        .fact(RUN_STATE_FACT)
        .and_then(|v| v.get("plan_digest").and_then(|d| d.as_str()))
        .and_then(|d| {
            if d.is_empty() {
                None
            } else {
                Some(d.to_string())
            }
        })
        .expect("the run state names the plan it started under");
    let plan = sven_harness_spec::compile_str(GREET).expect("compiles");
    assert_eq!(
        digest, plan.digest,
        "the recorded digest is the compiled plan's digest"
    );
    let _ = &m;
}

#[test]
fn a_checkpoint_resumes_at_the_step_it_stopped_on() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    assert!(
        matches!(m.step_effect(), Some(Effect::CallLlm { .. })),
        "the walk parks on the predict step"
    );
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );

    let snap = m.snapshot(&ctx);
    let (mut m2, mut ctx2) = HarnessMachine::restore(GREET.to_string(), snap)
        .expect("the unchanged document restores its own checkpoint");
    assert_eq!(
        ctx2.fact(RUN_STATE_FACT)
            .and_then(|v| v.get("current").and_then(|c| c.as_str())),
        Some("save"),
        "the checkpoint resumes at the stored step, not the entry"
    );
    let mut state = sven_harness_runtime::HarnessState::WaitingForModel;
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m2, &mut ctx2, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(
        m2.outcome().expect("finished")["message"],
        serde_json::json!("Hello Ada"),
        "the resumed run's answer is the one the checkpoint carried"
    );
}

#[test]
fn a_modified_document_cannot_silently_replace_a_checkpointed_plan() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("predict emitted its turn");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );

    let snap = m.snapshot(&ctx);
    let text = match HarnessMachine::restore(GREET_EDITED.to_string(), snap) {
        Err(err) => err.to_string(),
        Ok(_) => panic!("the edited document is not the checkpointed plan"),
    };
    assert!(
        text.contains("greet"),
        "the refusal names the harness: {text}"
    );
    assert!(
        text.contains("digest"),
        "the refusal says why: the plan's digest no longer matches: {text}"
    );
}
