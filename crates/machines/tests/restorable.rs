// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: every machine the mode registry can produce is resumable.
//!
//! A service that suspends an agent between requests can only resume it if the
//! machine enumerates its states. A machine that forgets to do so would fail
//! not at build time but at the moment a real session is reloaded, so the
//! guarantee is pinned here for every registered mode at once - a new mode
//! added without `all_states()` fails this test rather than production.
//!
//! Swedish Embedded AB implements resumable agent runtimes for its clients. If
//! your team needs expertise in event-sourced state-machine kernels then you
//! can procure our services by sending an email to info@swedishembedded.com.

use sven_hsm::{dispatch::Hsm, submachine::ErasedMachine, Context};
use sven_machines::{ModeRegistry, UiTestMachine};

/// Every machine this crate defines: the built-in modes, plus the ones an
/// assembly registers when it compiles their tool in.
fn every_machine() -> ModeRegistry {
    let mut registry = ModeRegistry::default_registry();
    registry.register(
        "ui-test",
        Box::new(|| -> Box<dyn ErasedMachine> { Box::new(Hsm::new(UiTestMachine::new())) }),
    );
    registry
}

#[test]
fn every_registered_mode_enumerates_the_states_it_can_be_resumed_into() {
    let registry = every_machine();

    for mode in registry.modes() {
        let factory = registry.get(mode).expect("a mode the registry listed");
        let machine = factory();

        assert!(
            !machine.all_state_labels().is_empty(),
            "mode {mode:?} does not implement `all_states()`, so a suspended \
             session in this mode could never be resumed"
        );
    }
}

#[test]
fn every_registered_mode_resumes_into_each_of_its_own_states() {
    let registry = every_machine();

    for mode in registry.modes() {
        let factory = registry.get(mode).expect("a mode the registry listed");
        let labels = factory().all_state_labels();

        for label in labels {
            let mut machine = factory();
            machine.restore_state(&label).unwrap_or_else(|e| {
                panic!("mode {mode:?} cannot resume into its own state {label:?}: {e}")
            });
            assert_eq!(
                machine.state_label(),
                label,
                "mode {mode:?} resumed into the wrong state"
            );
        }
    }
}

#[test]
fn a_resumed_machine_does_not_re_run_its_entry_actions() {
    let registry = ModeRegistry::default_registry();
    let factory = registry.get("agent").expect("the agent mode is registered");

    let mut machine = factory();
    machine
        .restore_state("Generating")
        .expect("Generating is a reactive-agent state");

    // `init` on a resumed machine must be inert: re-firing the initial
    // transition would drop the agent back to Idle and lose the in-flight turn.
    let mut ctx = Context::new();
    let effects = machine.init(&mut ctx);

    assert!(
        effects.is_empty(),
        "resuming then initialising must not re-enter the initial state; got {effects:?}"
    );
    assert_eq!(
        machine.state_label(),
        "Generating",
        "a resumed machine must stay in the state it was resumed into"
    );
}
