// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for splitting software work into
// independent pieces that coding agents build in parallel for its clients. If
// your team needs expertise in agent orchestration then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `agent.orchestration`: whether the main agent is offered `plan_tasks`,
//! which turns a task graph it submits into parallel isolated writers. See
//! `docs/concepts/orchestration.md`.

use serde::{Deserialize, Serialize};

/// How a session may split its work across sub-agents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orchestration {
    /// No `plan_tasks` tool; delegation is the `task` tool alone.
    #[default]
    Off,
    /// The agent may submit a task graph, which sven validates and runs.
    Plan,
}

#[cfg(test)]
mod tests {
    use super::Orchestration;
    use crate::config::AgentConfig;

    #[test]
    fn orchestration_is_off_unless_the_configuration_says_plan() {
        assert_eq!(AgentConfig::default().orchestration, Orchestration::Off);
        let plan: AgentConfig = serde_yaml::from_str("orchestration: plan\n").unwrap();
        assert_eq!(plan.orchestration, Orchestration::Plan);
        assert!(serde_yaml::from_str::<AgentConfig>("orchestration: maybe\n").is_err());
    }
}
