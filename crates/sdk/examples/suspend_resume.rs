// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The lifecycle a service actually needs: advance one step, persist, free.
//!
//! Between the two turns below the agent does not exist - only bytes on disk
//! do. That is the property that makes it viable to serve many users from one
//! process: nothing is held open waiting for somebody to type.
//!
//! Run with: `cargo run -p sven-sdk --example suspend_resume`

use sven_sdk::{AgentState, Engine};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Built once. In a service this lives for the process and is cloned into
    // each request; the model client and its connection pool are shared.
    let engine = Engine::builder().config(sven_config::load(None)?).build()?;

    let store = std::env::temp_dir().join("sven-sdk-example-agent.json");

    // ── Request 1 ────────────────────────────────────────────────────────────
    let mut agent = engine.agent("agent");
    let first = agent
        .send("Pick a number between 1 and 100 and remember it.")
        .await?
        .reply;
    println!("turn 1: {first}");

    // `suspend` consumes the agent: after this line nothing of the
    // conversation is in memory, only the bytes on disk.
    std::fs::write(&store, serde_json::to_vec(&agent.suspend())?)?;

    // ── Request 2, arbitrarily later ─────────────────────────────────────────
    let state: AgentState = serde_json::from_slice(&std::fs::read(&store)?)?;
    let mut resumed = engine.resume(state)?;
    let second = resumed.send("What number did you pick?").await?.reply;
    println!("turn 2: {second}");

    std::fs::remove_file(&store)?;
    Ok(())
}
