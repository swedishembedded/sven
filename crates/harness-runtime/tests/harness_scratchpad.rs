// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the host gives a run a private directory. Prompts, system texts and
//! gate arguments read its path as `{{ run.scratchpad }}`, a command gate also
//! finds it in `SVEN_SCRATCHPAD`, the run state records it (so a checkpoint
//! carries it), the document may name it as a template over the inputs, and a
//! host that gives none cannot run a workflow that reads it.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in keeping the files of an
//! automated run out of the repository it works on then you can procure our
//! services by sending an email to info@swedishembedded.com.

mod common;

use common::drive;
use serde_json::{json, Value};
use sven_harness_runtime::{HarnessMachine, HarnessRunData, HarnessRunState, HarnessState};
use sven_hsm::{Context, Effect, Event, Machine};

const DOC: &str = r#"
api_version: "2"
name: scratch
inputs: { base: string }
execution:
  limits: { model_turns: 4 }
  scratchpad: "{{ inputs.base }}/work"
agent:
  system: "Notes go to {{ run.scratchpad }}/notes.md."
gates:
  judge:
    command: [check, "{{ run.scratchpad }}/report.json"]
workflow:
  - name: work
    task: "Write the report into {{ run.scratchpad }}."
    verify: [judge]
"#;

fn started(data: HarnessRunData) -> (HarnessMachine, Context, HarnessState, Vec<Effect>) {
    let mut m = HarnessMachine::new(data);
    let mut ctx = Context::new();
    let mut state = m.initial();
    let fx = drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state, fx)
}

fn data(scratchpad: Option<&str>) -> HarnessRunData {
    let data = HarnessRunData::new(DOC, json!({ "base": "/host/scratch" })).expect("valid");
    match scratchpad {
        Some(dir) => data.with_scratchpad(dir),
        None => data,
    }
}

fn gate_args(ctx: &mut Context, m: &mut HarnessMachine, state: &mut HarnessState) -> Value {
    let fx = drive(
        m,
        ctx,
        state,
        common::turn("harness.scratch", "wrote the report"),
    );
    fx.iter()
        .find_map(|e| match e {
            Effect::CallTool { args, .. } => Some(args.clone()),
            _ => None,
        })
        .expect("a gate call")
}

#[test]
fn texts_and_gates_read_the_scratchpad_the_host_gave() {
    let (mut m, mut ctx, mut state, fx) = started(data(Some("/host/pad")));
    let prompt = fx
        .iter()
        .find_map(|e| match e {
            Effect::CallLlm { request } => Some(request.to_string()),
            _ => None,
        })
        .expect("a model turn");
    assert!(
        prompt.contains("Write the report into /host/pad."),
        "{prompt}"
    );
    assert!(
        prompt.contains("Notes go to /host/pad/notes.md."),
        "{prompt}"
    );

    let args = gate_args(&mut ctx, &mut m, &mut state);
    assert_eq!(args["steps"][0]["argv"][1], "/host/pad/report.json");
    assert_eq!(
        args["env"]["SVEN_SCRATCHPAD"], "/host/pad",
        "a command gate finds the directory in its environment"
    );
}

#[test]
fn the_run_state_records_the_scratchpad_so_a_checkpoint_carries_it() {
    let (_m, ctx, _state, _fx) = started(data(Some("/host/pad")));
    let recorded = HarnessRunState::load(&ctx).run_values();
    assert_eq!(recorded.scratchpad.as_deref(), Some("/host/pad"));
}

#[test]
fn the_document_scratchpad_is_rendered_over_the_inputs() {
    assert_eq!(
        data(None)
            .document_scratchpad()
            .expect("renders")
            .as_deref(),
        Some("/host/scratch/work")
    );
    let plain = DOC.replace("  scratchpad: \"{{ inputs.base }}/work\"\n", "");
    let none = HarnessRunData::new(&plain, json!({ "base": "x" })).expect("valid");
    assert_eq!(none.document_scratchpad().expect("renders"), None);
}

#[test]
fn a_host_that_gives_no_scratchpad_cannot_run_a_workflow_that_reads_it() {
    let (m, ctx, state, _fx) = started(data(None));
    assert_eq!(state, HarnessState::Failed);
    let outcome = m.outcome().expect("a failure outcome");
    assert_eq!(outcome["code"], "scratchpad_unavailable", "{outcome}");
    drop(ctx);
}
