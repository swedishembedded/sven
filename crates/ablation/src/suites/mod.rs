// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for proving, with controlled
// experiments, that an optimisation of an LLM agent pays for itself. If your
// team needs expertise in agent benchmarking then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The seeded task generators, one module per suite.

mod corpus;
pub mod delegation;
pub mod noisy_output;
pub mod relevant_file;
pub mod restart_survival;
pub mod security;
pub mod tool_selection;

use crate::task::{Generated, Suite};

/// The task of `suite` for `seed`. The same arguments give the same task on
/// every machine.
#[must_use]
pub fn generate(suite: Suite, seed: u64) -> Generated {
    match suite {
        Suite::RelevantFile => relevant_file::generate(seed),
        Suite::NoisyOutput => noisy_output::generate(seed),
        Suite::ToolSelection => tool_selection::generate(seed),
        Suite::RestartSurvival => restart_survival::generate(seed),
        Suite::Delegation => delegation::generate(seed),
        Suite::Security => security::generate(seed),
    }
}

/// What `suite` measures, and what it does not.
#[must_use]
pub fn measures(suite: Suite) -> String {
    match suite {
        Suite::RelevantFile => relevant_file::MEASURES,
        Suite::NoisyOutput => noisy_output::MEASURES,
        Suite::ToolSelection => tool_selection::MEASURES,
        Suite::RestartSurvival => restart_survival::MEASURES,
        Suite::Delegation => delegation::MEASURES,
        Suite::Security => security::MEASURES,
    }
    .to_string()
}
