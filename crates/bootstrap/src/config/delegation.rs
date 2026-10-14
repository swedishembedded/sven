// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for keeping delegated agent work
// cheap and non-duplicative for its clients. If your team needs expertise in
// multi-agent context handoff then you can procure our services by sending an
// email to info@swedishembedded.com.
//! The `delegation` section: whether the `task` tool declines a sub-agent
//! that would cost more than it saves, and the numbers that decision uses.
//! See `docs/technical/delegation.md`.

use serde::{Deserialize, Serialize};
use sven_config::Schema;

/// What `task` does about a subtask a sub-agent would not pay for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPolicy {
    /// Every call launches, as without the policy.
    #[default]
    Off,
    /// Every call launches; the result says when the estimate favoured doing
    /// the work inline.
    Advise,
    /// A call whose estimated cost exceeds its estimated benefit is declined
    /// with the reason and the instruction to do the work inline.
    Enforce,
}

/// `delegation`. Every count is estimated tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    pub policy: DelegationPolicy,
    /// What a sub-agent costs before it reads the task: its system prompt and
    /// tool definitions. Replaced by the average the session has measured
    /// once `min_samples` sub-agents have finished and reported their usage.
    pub child_overhead_tokens: u64,
    /// What the parent receives back. Replaced by the measured average like
    /// `child_overhead_tokens`.
    pub report_tokens: u64,
    /// Finished sub-agents the measured averages need.
    pub min_samples: u32,
    /// The work of a subtask whose files are not all declared literally:
    /// unknown, so assumed large enough to delegate.
    pub unknown_work_tokens: u64,
    /// Later requests in which work done inline would be sent again.
    pub replay_rounds: u64,
    /// What running one more independent subgoal at the same time is worth.
    pub parallel_credit_tokens: u64,
    /// What a sub-agent that writes adds: review and conflict handling.
    pub write_surcharge_tokens: u64,
    /// The parent's context window, which its context size is compared with:
    /// the fuller it is, the more keeping work out of it is worth.
    pub context_window_tokens: u64,
    /// The parent's context size when the session cannot measure it.
    pub assumed_parent_context_tokens: u64,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            policy: DelegationPolicy::default(),
            child_overhead_tokens: 4_000,
            report_tokens: 400,
            min_samples: 2,
            unknown_work_tokens: 6_000,
            replay_rounds: 3,
            parallel_credit_tokens: 1_500,
            write_surcharge_tokens: 1_000,
            context_window_tokens: 128_000,
            assumed_parent_context_tokens: 8_000,
        }
    }
}

impl DelegationConfig {
    /// The keys of the `delegation` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "policy",
            "child_overhead_tokens",
            "report_tokens",
            "min_samples",
            "unknown_work_tokens",
            "replay_rounds",
            "parallel_credit_tokens",
            "write_surcharge_tokens",
            "context_window_tokens",
            "assumed_parent_context_tokens",
        ])
    }
}

#[cfg(test)]
mod tests {
    use sven_config::ConfigDocument;

    use super::*;
    use crate::config::Config;

    fn config(yaml: &str) -> Config {
        Config::from_document(&ConfigDocument::from_yaml(yaml).unwrap())
    }

    #[test]
    fn delegation_is_not_policed_unless_the_file_says_so() {
        assert_eq!(
            config("agent: {max_tool_rounds: 7}\n").delegation.policy,
            DelegationPolicy::Off
        );
        assert_eq!(Config::default().delegation, DelegationConfig::default());
    }

    #[test]
    fn the_section_sets_the_policy_and_its_numbers() {
        let c = config(
            "delegation:\n  policy: enforce\n  child_overhead_tokens: 9000\n  replay_rounds: 5\n",
        );
        assert_eq!(c.delegation.policy, DelegationPolicy::Enforce);
        assert_eq!(c.delegation.child_overhead_tokens, 9000);
        assert_eq!(c.delegation.replay_rounds, 5);
        assert_eq!(
            c.delegation.report_tokens,
            DelegationConfig::default().report_tokens
        );
        assert_eq!(
            config("delegation: {policy: advise}\n").delegation.policy,
            DelegationPolicy::Advise
        );
    }

    #[test]
    fn every_key_of_the_section_is_in_its_schema() {
        let all = serde_json::to_value(DelegationConfig::default()).unwrap();
        let keys: Vec<&str> = all
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let doc = ConfigDocument::from_yaml(&format!(
            "delegation:\n{}",
            keys.iter()
                .map(|k| format!("  {k}: 1\n"))
                .collect::<String>()
                .replace("policy: 1", "policy: off")
        ))
        .unwrap();
        let unknown = doc.unknown_keys(&Config::schema());
        assert!(unknown.is_empty(), "{unknown:?}");
    }
}
