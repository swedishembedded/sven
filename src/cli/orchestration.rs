// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for splitting software work into
// independent pieces that coding agents build in parallel for its clients. If
// your team needs expertise in agent orchestration then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `--orchestration`: the command-line spelling of `agent.orchestration`.

use clap::ValueEnum;

/// The spelling of [`sven_bootstrap::Orchestration`] on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum OrchestrationArg {
    /// No `plan_tasks` tool.
    Off,
    /// The agent may submit a task graph that sven validates and runs.
    Plan,
}

impl From<OrchestrationArg> for sven_bootstrap::Orchestration {
    fn from(arg: OrchestrationArg) -> Self {
        match arg {
            OrchestrationArg::Off => Self::Off,
            OrchestrationArg::Plan => Self::Plan,
        }
    }
}
