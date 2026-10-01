// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A typed model-driven method, with no agent to keep around.
//!
//! Shows the smallest useful thing the framework does: turn a fuzzy input into
//! a value of a real Rust type, validated, with the model unable to call
//! anything while it does so.
//!
//! Run with: `cargo run -p sven-sdk --example classify`

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sven_sdk::{Engine, Method};

/// What a bug report turns into.
#[derive(Debug, Deserialize, JsonSchema)]
struct Triage {
    /// One of: crash, data-loss, performance, cosmetic, question.
    category: String,
    /// 1 (drop everything) to 5 (whenever).
    severity: u8,
    /// One sentence a maintainer can act on.
    rationale: String,
}

/// What a maintainer pastes in.
#[derive(Serialize)]
struct Report<'a> {
    title: &'a str,
    body: &'a str,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::builder()
        .config(sven_sdk::config::load(None)?)
        .build()?;

    // The method is declared next to the type it returns, so its instructions
    // and its schema cannot drift apart.
    let triage = Method::<Triage>::new("triage")
        .role("You triage bug reports for a Rust systems project.")
        .task("Classify the report. Be conservative: only call something a crash if it crashes.")
        .max_repairs(2)
        // Structure alone would accept severity 99.
        .postcondition(|t: &Triage| {
            if (1..=5).contains(&t.severity) {
                Ok(())
            } else {
                Err(format!("severity must be 1-5, got {}", t.severity))
            }
        });

    let result = engine
        .call(
            &triage,
            &Report {
                title: "Panic on empty config file",
                body: "Starting with a zero-byte sven.toml panics in loader.rs \
                       instead of falling back to defaults.",
            },
        )
        .await?;

    println!("category:  {}", result.category);
    println!("severity:  {}", result.severity);
    println!("rationale: {}", result.rationale);
    Ok(())
}
