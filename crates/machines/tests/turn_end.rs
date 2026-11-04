// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements agent runtimes whose frontends always know
// when an answer is over for its clients. If your team needs expertise in
// event-driven agent kernels then you can procure our services by sending an
// email to info@swedishembedded.com.

//! Spec: the reactive agent ends the person's turn itself.
//!
//! A reply without tool calls is not the end of a turn the machine goes on
//! with: it may ask the person a question first, ask the model again after a
//! transient failure, or take a standing goal's next step. Only the machine
//! knows, so its turns leave the end to it (`machine_ends_turn`) and it
//! announces `TurnComplete` exactly when it comes to rest.

use serde_json::json;
use sven_hsm::{dispatch::Hsm, Context, Effect, Event, ProposedToolCall, ToolCallId, UiEvent};
use sven_machines::machines::reactive_agent::{ReactiveState, CHAT_THREAD};
use sven_machines::ReactiveAgentMachine;

fn started(text: &str) -> (Hsm<ReactiveAgentMachine>, Context, Vec<Effect>) {
    let mut hsm = Hsm::new(ReactiveAgentMachine::new());
    let mut ctx = Context::new();
    hsm.init(&mut ctx);
    let out = hsm.dispatch(&Event::user_message(text), &mut ctx);
    (hsm, ctx, out.effects)
}

fn reply(text: &str) -> Event {
    Event::LlmTurnComplete {
        thread: CHAT_THREAD.to_string(),
        text: text.to_string(),
        tool_calls: vec![],
    }
}

fn announces_turn_end(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Announce { event } if **event == UiEvent::TurnComplete))
}

fn leaves_turn_end_to_machine(effect: &Effect) -> bool {
    matches!(effect, Effect::CallLlm { request } if request["machine_ends_turn"] == json!(true))
}

#[test]
fn every_turn_it_asks_for_leaves_the_end_to_the_machine() {
    let (mut hsm, mut ctx, first) = started("fix the bug");
    assert!(leaves_turn_end_to_machine(&first[0]), "{first:?}");

    let call_id = ToolCallId::new();
    hsm.dispatch(
        &Event::LlmTurnComplete {
            thread: CHAT_THREAD.to_string(),
            text: String::new(),
            tool_calls: vec![ProposedToolCall {
                call_id,
                name: "shell".into(),
                args: json!({"command": "ls"}),
                capability: sven_hsm::ToolCapability::ExecuteShell,
            }],
        },
        &mut ctx,
    );
    let next = hsm.dispatch(
        &Event::ToolSucceeded {
            call_id,
            observation: json!("ok"),
        },
        &mut ctx,
    );
    assert!(leaves_turn_end_to_machine(&next.effects[0]), "{next:?}");

    // The nudge after an empty reply is asked by the loop, not the machine.
    let nudged = hsm.dispatch(&reply(""), &mut ctx);
    assert!(leaves_turn_end_to_machine(&nudged.effects[0]), "{nudged:?}");
}

#[test]
fn a_final_answer_ends_the_turn_once_the_machine_is_at_rest() {
    let (mut hsm, mut ctx, _) = started("hi");
    let out = hsm.dispatch(&reply("Hello!"), &mut ctx);
    assert_eq!(hsm.state(), ReactiveState::Idle);
    assert!(announces_turn_end(&out.effects), "{:?}", out.effects);
    assert_eq!(out.effects.len(), 1, "{:?}", out.effects);
}

#[test]
fn a_question_the_machine_asks_first_keeps_the_turn_open() {
    let (mut hsm, mut ctx, _) = started("port the parser");
    let out = hsm.dispatch(
        &reply("Which parser generator should I use?\n- nom\n- pest\n"),
        &mut ctx,
    );
    assert_eq!(hsm.state(), ReactiveState::Generating);
    assert!(!announces_turn_end(&out.effects), "{:?}", out.effects);
}

#[test]
fn a_transient_failure_is_asked_again_without_ending_the_turn() {
    let (mut hsm, mut ctx, _) = started("hi");
    let out = hsm.dispatch(
        &Event::LlmFailed {
            error: format!("{}: connection reset", sven_hsm::TRANSIENT),
        },
        &mut ctx,
    );
    assert_eq!(hsm.state(), ReactiveState::Generating);
    assert!(!announces_turn_end(&out.effects), "{:?}", out.effects);
    assert!(leaves_turn_end_to_machine(&out.effects[0]));
}

#[test]
fn a_failure_that_ends_the_turn_announces_it() {
    let (mut hsm, mut ctx, _) = started("hi");
    let out = hsm.dispatch(
        &Event::LlmFailed {
            error: "the model refused".into(),
        },
        &mut ctx,
    );
    assert_eq!(hsm.state(), ReactiveState::Idle);
    assert!(announces_turn_end(&out.effects), "{:?}", out.effects);
}

#[test]
fn a_cancelled_turn_is_ended_by_its_abort_not_announced_again() {
    let (mut hsm, mut ctx, _) = started("hi");
    let out = hsm.dispatch(&Event::UserCancelled, &mut ctx);
    assert_eq!(hsm.state(), ReactiveState::Idle);
    assert!(!announces_turn_end(&out.effects), "{:?}", out.effects);
}
