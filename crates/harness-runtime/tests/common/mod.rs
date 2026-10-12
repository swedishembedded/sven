// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Shared drivers for the harness machine's integration tests: the test
//! boundary is `dispatch_state` itself — transitions and effects asserted
//! at the kernel boundary, entry handlers followed by hand (no executor in
//! a unit test), no I/O anywhere.

use sven_harness_runtime::{HarnessMachine, HarnessRunData};
use sven_hsm::{Context, Effect, Event, Machine, Reaction};

/// Follows a transition's entry effects, bouncing any self-addressed
/// `EmitInternal` round trip (the pure-primitive advance path) the way the
/// kernel's internal executor does. Asserts entry handlers never transition.
pub fn drive(
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
pub fn start(
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
pub fn turn(thread: &str, text: &str) -> Event {
    Event::LlmTurnComplete {
        thread: thread.into(),
        text: text.into(),
        tool_calls: vec![],
    }
}
