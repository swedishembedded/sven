// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Deterministic orchestration around model judgement.
//!
//! The workflow below is ordinary Rust: the order of the steps, the rule that
//! decides whether to proceed, and the acceptance check are all code. The model
//! is asked only for the two things code cannot do - interpreting a request and
//! proposing a change.
//!
//! The important line is the last one. `verify` is a deterministic function, so
//! a model that *claims* success cannot produce one: the claim is checked, and
//! a failed check rejects the result no matter how confident the prose was.
//!
//! Run with: `cargo run -p sven-sdk --example verified_workflow`

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sven_sdk::{Engine, Method};

/// The model's reading of what was asked.
#[derive(Debug, Deserialize, JsonSchema)]
struct Plan {
    /// The single file the change belongs in.
    file: String,
    /// What to do, in one sentence.
    intent: String,
}

/// The model's proposed change.
#[derive(Debug, Deserialize, JsonSchema)]
struct Proposal {
    /// The complete new contents of the file.
    contents: String,
    /// Why this satisfies the request.
    justification: String,
}

#[derive(Serialize)]
struct Request<'a> {
    ask: &'a str,
    available_files: &'a [&'a str],
}

#[derive(Serialize)]
struct PlanInput<'a> {
    intent: &'a str,
    current_contents: &'a str,
}

/// A deterministic acceptance check. No model involved, and no way for one to
/// talk its way past it.
fn verify(contents: &str) -> Result<(), String> {
    if contents.trim().is_empty() {
        return Err("the file would be empty".into());
    }
    if !contents.contains("SPDX-License-Identifier") {
        return Err("the file is missing its SPDX licence identifier".into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::builder().config(sven_config::load(None)?).build()?;

    let plan_step = Method::<Plan>::new("plan")
        .role("You plan small, surgical edits to a Rust codebase.")
        .task(
            "Decide which single file the request concerns, and state the intent in one sentence.",
        );

    let propose_step = Method::<Proposal>::new("propose")
        .role("You write careful, idiomatic Rust.")
        .task("Produce the complete new contents of the file. Preserve every existing licence header.");

    // Step order is control flow, not something the model decides.
    let plan = engine
        .call(
            &plan_step,
            &Request {
                ask: "add a doc comment explaining what this module is for",
                available_files: &["src/lib.rs"],
            },
        )
        .await?;
    println!("plan: {} — {}", plan.file, plan.intent);

    let current = "// SPDX-License-Identifier: Apache-2.0\npub fn hello() {}\n";
    let proposal = engine
        .call(
            &propose_step,
            &PlanInput {
                intent: &plan.intent,
                current_contents: current,
            },
        )
        .await?;

    // The mandatory step. A workflow that only *asks* the model whether it
    // verified something has not verified anything.
    match verify(&proposal.contents) {
        Ok(()) => println!("accepted:\n{}", proposal.contents),
        Err(why) => println!(
            "rejected: {why}\n(the model said: {})",
            proposal.justification
        ),
    }
    Ok(())
}
