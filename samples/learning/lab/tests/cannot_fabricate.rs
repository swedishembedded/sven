// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//! Spec: a sample cannot mark its own work correct, and cannot measure the
//! wrong weights.
//!
//! This file pins the only routes that exist. The other half of each rule is
//! structural and is enforced by the compiler rather than by an assertion:
//! `Verdict` has no public constructor and no public fields, `Outcome` has no
//! `solved()`, and `AdapterPath` can only be parsed out of a server's own log.
//! The shortest path to a fabricated success does not exist, so there is no
//! test here that "calling it fails" - there is nothing to call.

use std::collections::BTreeMap;

use sample_learning_lab::{AdapterPath, NoAdapterPath, Outcome, PredicateSet, ServedModel};

fn observed(pairs: &[(&str, bool)]) -> BTreeMap<String, bool> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

#[test]
fn the_only_route_to_a_scored_outcome_runs_through_the_declared_predicates() {
    let set = PredicateSet::new(["wrote_the_file", "left_others_alone"]).expect("two predicates");
    let verdict = set
        .evaluate(&observed(&[
            ("wrote_the_file", true),
            ("left_others_alone", true),
        ]))
        .expect("every declared predicate was evaluated");

    assert!(matches!(
        Outcome::from_verdict(&verdict),
        Outcome::Answered { solved: true }
    ));
}

#[test]
fn a_failed_episode_is_never_recorded_as_an_unsolved_task() {
    // The shortcut this prevents is `Err(_) => unsolved`. An episode that
    // could not run produced no evidence about the model in either direction,
    // and scoring it as a failure lets an infrastructure fault masquerade as a
    // model that could not do the work.
    let outcome = Outcome::from_episode::<String>(Err("the server went away".into()));
    assert!(matches!(outcome, Outcome::Errored { .. }));
}

#[test]
fn an_arm_cannot_be_measured_against_a_server_that_would_never_apply_its_adapter() {
    // Observed twice while building this harness, each time as a clean null
    // result rather than an error: the run completes, every request is
    // answered, and both arms measure the base weights.
    let quiet = "brain serve: scanning model dir\nbrain serve: ready\n";
    assert_eq!(
        AdapterPath::from_server_log(quiet, &ServedModel::qwen3()),
        Err(NoAdapterPath::NoWatcher),
        "a server with no adapter watcher cannot be the subject of a before/after comparison"
    );

    let real = "brain serve: watching <adapters-dir> for promoted LoRA adapters (brain/qwen3)\n";
    assert!(
        AdapterPath::from_server_log(real, &ServedModel::qwen3()).is_ok(),
        "the server's own watcher line is the proof"
    );
}
