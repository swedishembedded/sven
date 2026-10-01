// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Building your own surface from the event stream.
//!
//! The same `SessionEvent` stream the TUI and the headless runner consume is
//! available to any consumer, so a web service can stream progress to a browser
//! without reaching into a kernel crate or reimplementing the agent loop.
//!
//! Run with: `cargo run -p sven-sdk --example watch_events`

use sven_sdk::{Engine, SessionEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::builder()
        .config(sven_sdk::config::load(None)?)
        .build()?;

    let mut agent = engine.agent("agent");

    // Subscribe before sending: a receiver created afterwards sees only what is
    // still buffered.
    let mut events = agent.events();
    let renderer = tokio::spawn(async move {
        while let Ok(event) = events.recv().await {
            match event {
                SessionEvent::TextDelta(chunk) => print!("{chunk}"),
                SessionEvent::ToolCallStarted(call) => {
                    println!("\n  → {} {}", call.name, call.args);
                }
                SessionEvent::ToolCallFinished { call_id, .. } => {
                    println!("  ← {call_id} done");
                }
                SessionEvent::TokenUsage { input, output, .. } => {
                    println!("\n  [{input} in / {output} out]");
                }
                SessionEvent::TurnComplete => break,
                _ => {}
            }
        }
    });

    agent
        .send("List the Rust files in this directory and say how many there are.")
        .await?;
    renderer.await?;
    Ok(())
}
