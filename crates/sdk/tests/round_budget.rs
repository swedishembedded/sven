// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Spec: the tool-round budget the application configures reaches the
//! reactive machine the engine actually runs.
//!
//! `AgentConfig.max_tool_rounds` travels as the `agent.max_tool_rounds`
//! context fact seeded by `Agent::run_turn`, and the machine reads it when it
//! starts its turn. The seam spans three crates, so it needs an end-to-end
//! assertion: a run configured for 40 rounds must leave the loop state saying
//! 40, not the machine's own default of 16 - a silently wrong budget wraps
//! the turn early and the model is told the budget is exhausted when it is
//! not.

use std::sync::Arc;

use sven_model::ResponseEvent;
use sven_model_mock::ScriptedMockProvider;
use sven_sdk::config::Config;
use sven_sdk::Engine;

/// One scripted assistant reply.
fn says(text: &str) -> Vec<ResponseEvent> {
    vec![
        ResponseEvent::TextDelta(text.to_string()),
        ResponseEvent::Done,
    ]
}

#[tokio::test]
async fn the_configured_round_budget_reaches_the_machine() {
    let provider = Arc::new(ScriptedMockProvider::new(vec![says("done")]));
    let mut config = Config::default();
    config.agent.max_tool_rounds = 40;
    let engine = Engine::builder()
        .config(config)
        .model_provider(Arc::clone(&provider) as Arc<_>)
        .build()
        .expect("an engine builds on the configured budget");

    let mut agent = engine.agent("agent");
    let reply = agent.send("hello").await.expect("a reply");
    assert!(reply.contains("done"), "the scripted reply came back");

    let kernel = agent
        .state()
        .kernel()
        .cloned()
        .expect("kernel state after a turn");
    let raw = kernel
        .context
        .facts
        .get(sven_machines::machines::loop_core::LOOP_STATE_KEY)
        .cloned()
        .expect("the reactive machine stores its loop state");
    let loop_state: sven_machines::machines::loop_core::LoopState =
        serde_json::from_value(raw).expect("loop state is well-formed");
    assert_eq!(
        loop_state.max_rounds, 40,
        "the machine must run with the configured budget, not its default"
    );
    assert_eq!(
        kernel
            .context
            .facts
            .get(sven_machines::MAX_TOOL_ROUNDS_FACT),
        Some(&serde_json::json!(40)),
        "the seeded fact is the budget the machine read"
    );
}
