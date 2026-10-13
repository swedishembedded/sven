// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping LLM agent requests
// small for its clients. If your team needs expertise in tool-definition
// budgets for agents then you can procure our services by sending an email
// to info@swedishembedded.com.
//! Spec: every tool a session registers states what it is for in one line.
//!
//! A tool whose definition is not in a request is known to the model by that
//! line alone, so a tool without one cannot be found.

use std::sync::Arc;

use sven_bootstrap::{BuiltinTools, Config, RuntimeBuilder};

#[tokio::test]
async fn every_registered_tool_states_a_summary_that_fits_a_line() {
    // Skills, personas and memory come from the home directory.
    let home = tempfile::tempdir().expect("a home directory").into_path();
    std::env::set_var("HOME", &home);
    std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
    let mut config = Config::default();
    config.model.provider = "mock".into();
    config.model.name = "mock-model".into();
    let (runtime, handle, ..) = RuntimeBuilder::new(Arc::new(config), "agent")
        .with_builtin_tools(BuiltinTools::Coding)
        .build()
        .await
        .expect("a runtime builds");
    runtime.abort();

    let registry = handle.tool_registry();
    let mut lacking = Vec::new();
    for name in registry.names() {
        let tool = registry.get(&name).expect("registered");
        let summary = tool.summary();
        if summary.trim().is_empty() || summary.chars().count() > 100 || summary.contains('\n') {
            lacking.push(format!("{name}: {summary:?}"));
        }
    }
    assert!(
        lacking.is_empty(),
        "tools without a one-line summary of at most 100 characters: {lacking:?}"
    );
}
