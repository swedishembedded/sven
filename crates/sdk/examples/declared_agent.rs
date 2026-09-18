// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent declared as a Rust trait, where the documentation is the prompt.
//!
//! Nothing here is written twice. The role is the trait's doc comment, each
//! task is its method's doc comment, and each schema is derived from the return
//! type - so there is nowhere for the instructions and the implementation to
//! drift apart, because changing one *is* changing the other.
//!
//! Note which methods reach the model. `assess` has no body, so its body comes
//! from the model. `may_merge` has one, so it is ordinary code and no amount of
//! confident prose can talk its way past it.
//!
//! Run with: `cargo run -p sven-sdk --example declared_agent`

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sven_sdk::{agent, Engine};

/// What a review produces.
#[derive(Debug, Deserialize, JsonSchema)]
struct Assessment {
    /// Correctness risk, 0-100.
    risk: u8,
    /// The single most important thing a reviewer should look at.
    focus: String,
}

/// A change to look at.
#[derive(Serialize, JsonSchema)]
struct Change {
    diff: String,
}

/// You are a meticulous Rust reviewer. You never speculate about code you
/// cannot see, and you say so plainly when a diff is too small to judge.
#[agent]
trait Reviewer {
    /// Assess the change for correctness risk, and name the one thing a human
    /// reviewer should look at first.
    async fn assess(&self, change: Change) -> Assessment;

    /// Whether this may merge without a second pair of eyes.
    ///
    /// Deterministic on purpose: the merge rule is policy, so it belongs in
    /// code where it can be read, tested and enforced.
    fn may_merge(&self, assessment: &Assessment) -> bool {
        assessment.risk < 50
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let engine = Engine::builder().config(sven_config::load(None)?).build()?;

    let mut reviewer = Reviewer::new(&engine);

    let assessment = reviewer
        .assess(Change {
            diff: "-    if x > 0 {\n+    if x >= 0 {".into(),
        })
        .await?;

    println!("risk:  {}", assessment.risk);
    println!("focus: {}", assessment.focus);
    println!(
        "merge: {}",
        if reviewer.may_merge(&assessment) {
            "allowed"
        } else {
            "needs review"
        }
    );
    Ok(())
}
