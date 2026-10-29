// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `sven harness run --model-role`: each role a workflow names is answered by
//! the model mapped to it, and a run whose roles are not mapped exactly is
//! refused before any model is asked.
//!
//! Swedish Embedded AB implements multi-model agent workflows for its
//! clients. If your team needs expertise in routing agent tasks to the right
//! model then you can procure our services by sending an email to
//! info@swedishembedded.com.

mod harness_support;

use harness_support::cli::fixtures;
use harness_support::{code, report, run, serve, setup, stderr, Turn};

#[test]
fn each_role_is_answered_by_the_model_mapped_to_it() {
    let setup = setup();
    let server = serve(vec![Turn::Text("step one"), Turn::Text("done")]);
    let out = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"])
        .args(["--model-role", "planner=openai/scripted-planner"]));
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(report(&out)["output"]["result"], "done");
    assert_eq!(
        server.seen.lock().unwrap().models,
        ["scripted-planner", "scripted-coder"],
        "the planner role's turn goes to its model, the unroled turn to --model"
    );
}

#[test]
fn an_unmapped_or_unknown_role_is_refused_before_any_model_is_asked() {
    let setup = setup();
    let server = serve(vec![]);
    let unmapped = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"]));
    assert_eq!(code(&unmapped), 2, "{}", stderr(&unmapped));
    assert!(
        stderr(&unmapped).contains("planner"),
        "{}",
        stderr(&unmapped)
    );

    let unknown = run(setup
        .subcommand("run", &fixtures().join("roles"), &server)
        .args(["--approvals", "deny"])
        .args(["--model-role", "planner=openai/scripted-planner"])
        .args(["--model-role", "planer=openai/typo"]));
    assert_eq!(code(&unknown), 2, "{}", stderr(&unknown));
    assert!(stderr(&unknown).contains("planer"), "{}", stderr(&unknown));
    assert_eq!(server.seen.lock().unwrap().requests, 0);
}
