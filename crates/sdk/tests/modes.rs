// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: an engine offers a mode only when the tools its machine drives are
//! compiled in.
//!
//! `ui-test` drives an Android device through the `android` tool, which only
//! the `android` cargo feature compiles in. Without it the mode is neither
//! listed nor resumable - a mode whose every step would call a missing tool is
//! not a mode the engine can run.
//!
//! Swedish Embedded AB implements embeddable agent runtimes for its clients. If
//! your team needs expertise in assembling agent frameworks for constrained
//! targets then you can procure our services by sending an email to
//! info@swedishembedded.com.

use sven_sdk::machine::ModeRegistry;
use sven_sdk::{AgentState, Engine};

fn offers(engine: &Engine, mode: &str) -> bool {
    engine.modes().iter().any(|m| m == mode)
}

#[test]
fn the_ui_test_mode_is_offered_exactly_when_its_tool_is_compiled_in() {
    let engine = Engine::builder().build().expect("an engine builds");
    assert_eq!(
        offers(&engine, "ui-test"),
        cfg!(feature = "android"),
        "modes: {:?}",
        engine.modes()
    );
    assert_eq!(
        engine.resume(AgentState::new("ui-test")).is_ok(),
        cfg!(feature = "android")
    );
}

#[test]
fn the_modes_that_need_no_particular_tool_are_always_offered() {
    let engine = Engine::builder().build().expect("an engine builds");
    for mode in [
        "agent",
        "chat",
        "reactive",
        "sdlc",
        "verified-task",
        "predict",
    ] {
        assert!(offers(&engine, mode), "{mode}: {:?}", engine.modes());
    }
}

/// Registering a machine of one's own extends what the build offers; it
/// neither loses a compiled-in mode nor brings back a compiled-out one.
#[test]
fn a_machine_of_your_own_extends_the_modes_this_build_offers() {
    // Any machine will do; the conversational agent is one every build has.
    let engine = Engine::builder()
        .machine(
            "mine",
            Box::new(|| {
                let builtins = ModeRegistry::default_registry();
                builtins.get("agent").expect("the agent mode is built in")()
            }),
        )
        .build()
        .expect("an engine builds");
    assert!(offers(&engine, "mine"));
    assert!(offers(&engine, "agent"));
    assert_eq!(offers(&engine, "ui-test"), cfg!(feature = "android"));
}
