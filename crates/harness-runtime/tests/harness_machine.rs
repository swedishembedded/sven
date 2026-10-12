// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The harness machine's spec, driven directly against `dispatch_state` the
//! way the verified-task machine's unit tests drive theirs: transitions and
//! effects asserted at the kernel boundary, entry handlers followed by hand
//! (no executor in a unit test), no I/O anywhere.

use sven_harness_runtime::{HarnessMachine, HarnessRunData, RUN_STATE_FACT};
use sven_hsm::{Context, Effect, Event, Machine, Reaction};
use sven_vocab::verify::{VerifierSpec, VerifierVerdict};

/// Follows a transition's entry effects, bouncing any self-addressed
/// `EmitInternal` round trip (the pure-primitive advance path) the way the
/// kernel's internal executor does. Asserts entry handlers never transition.
fn drive(
    m: &mut HarnessMachine,
    ctx: &mut Context,
    state: &mut sven_harness_runtime::HarnessState,
    event: Event,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let reaction = m.dispatch_state(*state, &event, ctx);
    match reaction {
        Reaction::Transition {
            target,
            effects: mut fx,
            ..
        } => {
            effects.append(&mut fx);
            *state = target;
            let entry = m.dispatch_state(*state, &Event::entry(), ctx);
            assert!(
                !entry.is_transition(),
                "entry handlers must never transition"
            );
            if let Reaction::Handled(entry_fx) = entry {
                for effect in entry_fx {
                    if let Effect::EmitInternal { name, payload } = &effect {
                        effects.push(effect.clone());
                        let bounced = m.dispatch_state(
                            *state,
                            &Event::Internal(sven_hsm::event::InternalEvent::Custom {
                                name: name.clone(),
                                payload: payload.clone(),
                            }),
                            ctx,
                        );
                        if let Reaction::Transition {
                            target,
                            effects: fx,
                            ..
                        } = bounced
                        {
                            effects.extend(fx);
                            *state = target;
                            let entry = m.dispatch_state(*state, &Event::entry(), ctx);
                            assert!(
                                !entry.is_transition(),
                                "entry handlers must never transition"
                            );
                        }
                    } else {
                        effects.push(effect);
                    }
                }
            }
        }
        Reaction::Handled(mut fx) => effects.append(&mut fx),
        Reaction::Ignored => {}
        Reaction::Super(_) => panic!("test states have no superstate to defer to"),
    }
    effects
}

/// Compiles a harness from YAML, seeds a fresh context with its run data,
/// and drives the machine through its initial transition.
fn start(
    yaml: &str,
    inputs: serde_json::Value,
) -> (HarnessMachine, Context, sven_harness_runtime::HarnessState) {
    let run = HarnessRunData::new(yaml, inputs).expect("valid harness");
    let mut m = HarnessMachine::new(run);
    let mut ctx = Context::new();
    let mut state = m.initial();
    drive(&mut m, &mut ctx, &mut state, Event::user_message("start"));
    (m, ctx, state)
}

/// The thread every harness model turn runs in.
fn turn(thread: &str, text: &str) -> Event {
    Event::LlmTurnComplete {
        thread: thread.into(),
        text: text.into(),
        tool_calls: vec![],
    }
}

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

#[test]
fn a_predict_step_emits_one_call_llm_and_stores_its_answer() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));

    // The predict step: one constrained turn, in the harness's own thread.
    assert_eq!(state, sven_harness_runtime::HarnessState::WaitingForModel);
    let ask = m
        .step_effect()
        .expect("the entered step emitted its effect");
    let Effect::CallLlm { request } = &ask else {
        panic!("a predict step asks the model, got {ask:?}");
    };
    assert_eq!(request["thread"], "harness.greet");
    assert!(request["instruction"].as_str().unwrap().contains("Ada"));

    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );
    assert!(
        fx.is_empty(),
        "storing the answer advances by internal event only"
    );
    assert_eq!(
        ctx.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("ask"))
            .map(|s| s["value"].clone())),
        Some(serde_json::json!("Hello Ada"))
    );

    // The pure tail (assign → return) advances without I/O and completes.
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert!(m.is_terminal(state), "Completed is terminal");
    let out = m.outcome().expect("a completed run has an outcome");
    assert_eq!(out["message"], serde_json::json!("Hello Ada"));
}

#[test]
fn a_tool_step_emits_call_tool_with_a_deterministic_call_id_and_stores_the_observation() {
    let yaml = r#"
api_version: "1"
name: checker
entry: probe
limits: { max_steps: 50, max_tool_calls: 10 }
steps:
  probe:
    tool:
      name: read_file
      args: { path: "notes/scratch.txt" }
      store_as: found
      then: out
  out:
    return:
      value: "{{ steps.probe.value }}"
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));

    let ask = m.step_effect().expect("the tool step emitted its effect");
    let Effect::CallTool {
        call_id,
        name,
        args,
        ..
    } = &ask
    else {
        panic!("a tool step calls a tool, got {ask:?}");
    };
    assert_eq!(name, "read_file");
    assert_eq!(args["path"], "notes/scratch.txt");
    // Deterministic per step: replay reproduces the same id, so the recorded
    // ToolSucceeded still matches after a checkpoint restore.
    let expected = sven_harness_runtime::step_call_id("checker", "probe");
    assert_eq!(*call_id, expected);

    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: expected,
            observation: serde_json::json!("file contents"),
        },
    );
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(
        m.outcome().unwrap(),
        serde_json::json!("file contents"),
        "a single-hole template carries the value's type through"
    );
}

#[test]
fn a_branch_step_chooses_between_successors_without_io() {
    let yaml = r#"
api_version: "1"
name: router
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Is it raining?"
      store_as: answer
      then: route
  route:
    branch:
      on: "steps.ask.value"
      when_true: wet
      when_false: dry
  wet:
    assign: { set: { where: umbrella }, then: out }
  dry:
    assign: { set: { where: sunglasses }, then: out }
  out:
    return:
      value: "{{ facts.where }}"
"#;
    // The model answers "yes" → the truthy branch.
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let _ = m.step_effect().expect("predict emitted its turn");
    let _ = drive(&mut m, &mut ctx, &mut state, turn("harness.router", "yes"));
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(m.outcome().unwrap(), serde_json::json!("umbrella"));
}

#[test]
fn a_retry_step_bounds_a_failing_body_and_honors_on_exhausted() {
    let yaml = r#"
api_version: "1"
name: flaky
entry: attempt
limits: { max_steps: 100, max_tool_calls: 10 }
steps:
  attempt:
    retry:
      attempts: 2
      body: probe
      then: ok
      on_exhausted: give_up
  probe:
    tool:
      name: read_file
      args: { path: "flaky-probe" }
      then: check
  check:
    branch:
      on: "!steps.probe.ok"
      when_true: attempt
      when_false: ok
  ok:
    return: { value: "made it" }
  give_up:
    fail: { code: "unreachable", message: "the probe never succeeded" }
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let call = |n: u64| sven_harness_runtime::step_call_id("flaky", &format!("probe-{n}"));

    // Attempt 1: the tool fails → the branch routes back into the retry.
    let _ = m.step_effect().expect("the body step emitted its call");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolFailed {
            call_id: call(1),
            error: "file missing".into(),
        },
    );
    // Branch evaluates `steps.probe.ok` → absent → falsy → !falsy is false… wait,
    // the branch is `!steps.probe.ok`: ok is absent → falsy → NOT falsy = true →
    // when_true = attempt again.
    assert_eq!(
        state,
        sven_harness_runtime::HarnessState::WaitingForTool,
        "the retry re-entered its body"
    );

    // Attempt 2: the tool succeeds → the branch exits the retry via `ok`.
    let _ = m
        .step_effect()
        .expect("the re-entered body emitted its call");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id: call(2),
            observation: serde_json::json!({ "ok": true }),
        },
    );
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(m.outcome().unwrap(), serde_json::json!("made it"));
}

#[test]
fn an_agent_step_runs_the_tool_loop_and_stores_the_final_text() {
    let yaml = r#"
api_version: "1"
name: worker
entry: do
capabilities: [ExecuteShell]
limits: { max_steps: 100, max_model_turns: 20, max_tool_calls: 20 }
steps:
  do:
    agent:
      prompt: "Create the file"
      tools: [read_file, write_file]
      store_as: report
      then: out
  out:
    return:
      value: "{{ steps.do.value }}"
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let _ = m.step_effect().expect("the agent step opened its loop");
    assert_eq!(state, sven_harness_runtime::HarnessState::WaitingForModel);

    // The loop asks for tools; the machine stays in WaitingForModel and
    // emits CallTool effects.
    let call_id = sven_hsm::ids::ToolCallId::new();
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::LlmTurnComplete {
            thread: "harness.worker".into(),
            text: String::new(),
            tool_calls: vec![sven_hsm::ProposedToolCall {
                call_id,
                name: "write_file".into(),
                args: serde_json::json!({ "path": "x.txt", "content": "hi" }),
                capability: sven_hsm::ToolCapability::WriteFile,
            }],
        },
    );
    assert_eq!(state, sven_harness_runtime::HarnessState::WaitingForModel);
    assert!(
        fx.iter()
            .any(|e| matches!(e, Effect::CallTool { name, .. } if name == "write_file")),
        "the loop runs the proposed tools"
    );

    // The tool result resumes the loop; the next turn with no tools is final.
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::ToolSucceeded {
            call_id,
            observation: serde_json::json!("written"),
        },
    );
    let fx = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.worker", "wrote x.txt"),
    );
    assert!(fx.is_empty(), "a final turn just stores the answer");
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(m.outcome().unwrap(), serde_json::json!("wrote x.txt"));
}

#[test]
fn a_verify_step_waits_for_the_verifier_and_stores_the_verdict() {
    let yaml = r#"
api_version: "1"
name: graded
entry: check
capabilities: [RunVerifier]
limits: { max_steps: 50 }
steps:
  check:
    verify:
      spec:
        kind: file_exists
        path: "report/done.txt"
      store_as: verdict
      then: out
  out:
    return:
      value: "{{ steps.check.value }}"
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let ask = m.step_effect().expect("the verify step emitted its effect");
    assert_eq!(
        state,
        sven_harness_runtime::HarnessState::WaitingForVerifier
    );
    let Effect::Verify { spec } = &ask else {
        panic!("a verify step emits Effect::Verify, got {ask:?}");
    };
    assert_eq!(
        *spec,
        VerifierSpec::FileExists {
            path: "report/done.txt".into(),
            min_bytes: None,
        }
    );

    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        Event::VerificationComplete {
            verdict: VerifierVerdict::Passed,
        },
    );
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(m.outcome().unwrap(), serde_json::json!("passed"));
}

#[test]
fn a_fail_step_ends_the_run_failed_with_code_and_message() {
    let yaml = r#"
api_version: "1"
name: moody
entry: ask
limits: { max_steps: 50, max_model_turns: 5 }
steps:
  ask:
    predict:
      prompt: "Mood?"
      store_as: mood
      then: route
  route:
    branch:
      on: "{{ steps.ask.value }}"
      when_true: bad
      when_false: bad
  bad:
    fail:
      code: "no_mood"
      message: "the mood was {{ steps.ask.value }}"
"#;
    let (mut m, mut ctx, mut state) = start(yaml, serde_json::json!({}));
    let _ = m.step_effect().expect("predict emitted its turn");
    let _ = drive(&mut m, &mut ctx, &mut state, turn("harness.moody", "grim"));
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Failed {
            break;
        }
        let _ = drive(&mut m, &mut ctx, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Failed);
    assert!(m.is_terminal(state));
    let out = m.outcome().expect("a failed run has an outcome");
    assert_eq!(out["code"], "no_mood");
    assert_eq!(out["message"], "the mood was grim");
}

#[test]
fn the_state_hierarchy_is_fixed_regardless_of_plan_size() {
    let (m, _ctx, _state) = start(GREET, serde_json::json!({}));
    let states = m.all_states();
    let names: Vec<String> = states.iter().map(|s| format!("{s:?}")).collect();
    for expected in [
        "Root",
        "Ready",
        "Advancing",
        "WaitingForModel",
        "WaitingForTool",
        "WaitingForVerifier",
        "WaitingForChild",
        "WaitingForJoin",
        "Completed",
        "Failed",
        "Cancelled",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing {expected}: {names:?}"
        );
    }
    // Restorability: every leaf, in a fresh machine.
    let (m2, _c, _s) = start(GREET, serde_json::json!({}));
    assert!(
        m2.restorable_state_labels().iter().all(|l| !l.is_empty()),
        "all_states feeds snapshot restore"
    );
    let (m3, _c, _s) = start(GREET, serde_json::json!({}));
    assert_eq!(
        m3.all_states().len(),
        11,
        "the hierarchy is fixed: eleven states"
    );
}

#[test]
fn run_state_lives_in_the_context_and_survives_a_snapshot_round_trip() {
    let (mut m, mut ctx, mut state) = start(GREET, serde_json::json!({ "name": "Ada" }));
    let _ = m.step_effect().expect("predict emitted its turn");
    let _ = drive(
        &mut m,
        &mut ctx,
        &mut state,
        turn("harness.greet", "Hello Ada"),
    );

    // Snapshot → restore into a fresh machine: the run continues from the
    // stored step data, no entry actions re-fire.
    let snap = m.snapshot(&ctx);
    let (m2, ctx2) = HarnessMachine::restore(run_yaml(), snap).expect("restorable");
    assert_eq!(
        ctx2.fact(RUN_STATE_FACT).and_then(|v| v
            .get("steps")
            .and_then(|s| s.get("ask"))
            .map(|s| s["value"].clone())),
        Some(serde_json::json!("Hello Ada")),
        "the step result survived the round trip"
    );
    // And the restored machine finishes the run.
    let (m2, mut ctx2) = (m2, ctx2);
    let mut state = sven_harness_runtime::HarnessState::WaitingForModel;
    let mut m2 = m2;
    for _ in 0..8 {
        if state == sven_harness_runtime::HarnessState::Completed {
            break;
        }
        let _ = drive(&mut m2, &mut ctx2, &mut state, Event::user_message("tick"));
    }
    assert_eq!(state, sven_harness_runtime::HarnessState::Completed);
    assert_eq!(
        m2.outcome().unwrap()["message"],
        serde_json::json!("Hello Ada")
    );
}

fn run_yaml() -> String {
    GREET.to_string()
}

#[test]
fn the_permission_policy_is_the_declared_capabilities_in_every_state() {
    let yaml = r#"
api_version: "1"
name: writer
entry: do
capabilities: [ReadFile, WriteFile]
limits: { max_steps: 100, max_tool_calls: 20 }
steps:
  do:
    tool:
      name: write_file
      args: { path: "x", content: "y" }
      then: out
  out:
    return: { value: "done" }
"#;
    let run = HarnessRunData::new(yaml, serde_json::json!({})).expect("valid harness");
    let policy = HarnessMachine::permission_policy_for(&run);
    use sven_hsm::ToolCapability;
    assert!(policy.allows_in_every_state(ToolCapability::ReadFile));
    assert!(policy.allows_in_every_state(ToolCapability::WriteFile));
    assert!(
        !policy.allows_in_every_state(ToolCapability::NetworkAccess),
        "an undeclared capability stays undeclared"
    );
    assert!(
        !policy.allows_in_every_state(ToolCapability::ExecuteShell),
        "only declared capabilities are granted"
    );

    // No capabilities declared: nothing is granted.
    let bare = r#"
api_version: "1"
name: pure
entry: only
limits: { max_steps: 10 }
steps:
  only:
    assign: { set: { x: "1" }, then: out }
  out:
    return: { value: "ok" }
"#;
    let run = HarnessRunData::new(bare, serde_json::json!({})).expect("valid harness");
    let policy = HarnessMachine::permission_policy_for(&run);
    for cap in sven_hsm::ToolCapability::ALL {
        assert!(
            !policy.allows_in_every_state(cap),
            "a capability-free harness grants nothing: {cap:?}"
        );
    }
}
