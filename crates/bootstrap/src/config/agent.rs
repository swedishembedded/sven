// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `agent:` section of the configuration file: how a session's loop is
//! bounded, when it compacts, and how long a model may keep it waiting.
//!
//! Swedish Embedded AB implements bounded, auditable agent loops for its
//! clients. If your team needs expertise in running language-model agents
//! within budgets you can trust then you can procure our services by sending
//! an email to info@swedishembedded.com.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sven_config::Schema;
use sven_executors::CompactionConfig;
use sven_turn::{CompactionStrategy, ThinkingBudget, TurnLimits};
use sven_vocab::AgentMode;

fn default_agent_mode() -> AgentMode {
    AgentMode::Agent
}
fn default_max_tool_rounds() -> u32 {
    200
}
fn default_compaction_threshold() -> f32 {
    0.85
}
fn default_child_run_timeout_secs() -> u64 {
    3600
}
fn default_compaction_keep_recent() -> usize {
    6
}
fn default_tool_result_token_cap() -> usize {
    4000
}
fn default_compaction_overhead_reserve() -> f32 {
    0.10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Default mode when none is specified on the CLI
    #[serde(default = "default_agent_mode")]
    pub default_mode: AgentMode,
    /// Maximum number of autonomous tool-call rounds before stopping
    #[serde(default = "default_max_tool_rounds")]
    pub max_tool_rounds: u32,
    /// Token fraction at which proactive compaction triggers (0.0-1.0),
    /// checked by `TurnExecutor` before every turn against the usable input
    /// budget (`sven_model::budget::effective_input_budget`), minus
    /// `compaction_overhead_reserve`. See `sven_turn::prepare_compaction`.
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
    /// Number of recent non-system messages preserved verbatim during
    /// compaction.  The oldest messages beyond this tail are summarised by
    /// the LLM.  Higher values retain more recent context but reduce the
    /// compression benefit.
    ///
    /// A value of 6 corresponds to roughly 3 back-and-forth turns
    /// (user + assistant per turn, tool results excluded from the count).
    /// Set to 0 to summarise the full history (original behaviour).
    #[serde(default = "default_compaction_keep_recent")]
    pub compaction_keep_recent: usize,
    /// Compaction checkpoint format.
    ///
    /// `structured` (default): produces a typed Markdown checkpoint with
    /// fixed sections preserving tasks, decisions, files, and constraints.
    /// `narrative`: uses the original free-form summarisation prompt.
    #[serde(default)]
    pub compaction_strategy: CompactionStrategy,
    /// Maximum tokens allowed for a single tool result before it is
    /// deterministically truncated before entering the session, applied by
    /// `ToolExecutor` on the way into the conversation store (`sven_machines::
    /// smart_truncate`; category comes from the tool's own
    /// `Tool::output_category()`).
    ///
    /// Truncation is content-aware: shell output keeps head+tail lines, grep
    /// keeps leading matches, read_file keeps head+tail lines.  A value of
    /// 0 disables per-result truncation entirely.
    ///
    /// Only affects what's stored for the model's next turn - the full,
    /// untruncated output still reaches `UiEvent::ToolFinished` (TUI
    /// display) and the audit trail.
    #[serde(default = "default_tool_result_token_cap")]
    pub tool_result_token_cap: usize,
    /// Fraction of the context window reserved for tool schemas, the dynamic
    /// context block (git/CI info), and measurement error in the token
    /// approximation.  Reduces the effective compaction trigger threshold.
    ///
    /// Example: threshold=0.85, reserve=0.10 → compaction fires when
    /// calibrated session tokens reach 75% of the input budget.
    #[serde(default = "default_compaction_overhead_reserve")]
    pub compaction_overhead_reserve: f32,
    /// System prompt override; leave None to use the built-in prompt
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Per-step wall-clock timeout in seconds (0 = no limit).
    /// Can be set in config, overridden by frontmatter or CLI flag.
    #[serde(default)]
    pub max_step_timeout_secs: u64,

    /// Total run wall-clock timeout in seconds (0 = no limit).
    #[serde(default)]
    pub max_run_timeout_secs: u64,

    /// Cap on estimated thinking/reasoning tokens for a single turn, guarding
    /// against a model (observed with some local reasoning models, e.g. Qwen)
    /// that loops indefinitely instead of converging to an answer. Checked by
    /// `stream_turn` against a chars/4 estimate of accumulated
    /// `ThinkingDelta` content. `None` (the default) falls back to 10% of the
    /// model's resolved context window - or no cap at all when the window
    /// isn't known. Live-adjustable in a session via `/think-limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_thinking_tokens: Option<u32>,

    /// Cap on wall-clock time a model may spend emitting `ThinkingDelta`s
    /// with no forward progress (a real text or tool-call delta) before the
    /// turn is aborted - the other half of the thinking-loop watchdog beside
    /// `max_thinking_tokens`, whichever fires first. Reset on every sign of
    /// progress, so a legitimately long multi-step turn is never killed.
    /// `None` (the default) falls back to 600s. Live-adjustable via
    /// `/think-limit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_timeout_secs: Option<u64>,

    /// Longest silence, in seconds, between two chunks of a streamed model
    /// response before the connection is declared stale and the turn fails.
    /// Raise it for a slow but live provider, such as CPU prefill of a long
    /// prompt. `None` (the default) is 300s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_idle_timeout_secs: Option<u64>,

    /// Wall-clock budget, in seconds, of every child agent run this session
    /// starts: a parallel SDLC task, a `task` sub-agent. The child is stopped
    /// when it runs out, and never outlives its parent. 0 = no limit.
    #[serde(default = "default_child_run_timeout_secs")]
    pub child_run_timeout_secs: u64,
}

impl AgentConfig {
    /// The keys of the `agent:` section.
    #[must_use]
    pub fn schema() -> Schema {
        Schema::keys(&[
            "default_mode",
            "max_tool_rounds",
            "compaction_threshold",
            "compaction_keep_recent",
            "compaction_strategy",
            "tool_result_token_cap",
            "compaction_overhead_reserve",
            "system_prompt",
            "max_step_timeout_secs",
            "max_run_timeout_secs",
            "max_thinking_tokens",
            "thinking_timeout_secs",
            "stream_idle_timeout_secs",
            "child_run_timeout_secs",
        ])
    }

    /// The wall-clock budget of a child run, if bounded.
    #[must_use]
    pub fn child_run_timeout(&self) -> Option<Duration> {
        (self.child_run_timeout_secs > 0).then(|| Duration::from_secs(self.child_run_timeout_secs))
    }

    /// The limits one streamed model turn runs under. A `None` thinking
    /// limit stays `None`: its default depends on the model in use, which is
    /// only known once a turn has one.
    #[must_use]
    pub fn turn_limits(&self) -> TurnLimits {
        TurnLimits::from(ThinkingBudget {
            max_thinking_tokens: self.max_thinking_tokens,
            thinking_timeout_secs: self.thinking_timeout_secs,
        })
        .with_stream_idle_secs(self.stream_idle_timeout_secs)
    }

    /// When and how a session compacts its history.
    #[must_use]
    pub fn compaction(&self) -> CompactionConfig {
        CompactionConfig {
            threshold: self.compaction_threshold,
            overhead_reserve: self.compaction_overhead_reserve,
            keep_recent: self.compaction_keep_recent,
            strategy: self.compaction_strategy.clone(),
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_mode: AgentMode::Agent,
            max_tool_rounds: 200,
            compaction_threshold: 0.85,
            compaction_keep_recent: default_compaction_keep_recent(),
            compaction_strategy: CompactionStrategy::Structured,
            tool_result_token_cap: default_tool_result_token_cap(),
            compaction_overhead_reserve: default_compaction_overhead_reserve(),
            system_prompt: None,
            max_step_timeout_secs: 0,
            max_run_timeout_secs: 0,
            child_run_timeout_secs: default_child_run_timeout_secs(),
            max_thinking_tokens: None,
            thinking_timeout_secs: None,
            stream_idle_timeout_secs: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_values() {
        let a = AgentConfig::default();
        assert_eq!(a.default_mode, AgentMode::Agent);
        assert_eq!(a.max_tool_rounds, 200);
        assert!((a.compaction_threshold - 0.85).abs() < f32::EPSILON);
        assert_eq!(a.compaction_keep_recent, 6);
        assert_eq!(a.compaction_strategy, CompactionStrategy::Structured);
        assert!(a.system_prompt.is_none());
    }

    #[test]
    fn a_partial_section_keeps_the_defaults_for_absent_keys() {
        let a: AgentConfig = serde_yaml::from_str("compaction_keep_recent: 12\n").unwrap();
        assert_eq!(a.compaction_keep_recent, 12);
        assert_eq!(a.max_tool_rounds, 200);
        assert_eq!(a.compaction_strategy, CompactionStrategy::Structured);
    }

    #[test]
    fn the_mode_and_strategy_are_written_in_lowercase() {
        let a: AgentConfig =
            serde_yaml::from_str("default_mode: plan\ncompaction_strategy: narrative\n").unwrap();
        assert_eq!(a.default_mode, AgentMode::Plan);
        assert_eq!(a.compaction_strategy, CompactionStrategy::Narrative);
    }

    #[test]
    fn the_thinking_watchdog_keys_reach_the_turn_limits() {
        let a: AgentConfig = serde_yaml::from_str(
            "max_thinking_tokens: 4096\nthinking_timeout_secs: 90\nstream_idle_timeout_secs: 7\n",
        )
        .unwrap();
        let limits = a.turn_limits();
        assert_eq!(limits.thinking.max_thinking_tokens, Some(4096));
        assert_eq!(limits.thinking.thinking_timeout_secs, Some(90));
        assert_eq!(limits.stream_idle, Duration::from_secs(7));
    }

    #[test]
    fn the_thinking_watchdog_defaults_to_the_models_own_limits() {
        let limits = AgentConfig::default().turn_limits();
        assert_eq!(limits, TurnLimits::default());
        assert_eq!(limits.thinking, ThinkingBudget::default());
    }

    #[test]
    fn the_compaction_keys_reach_the_executor_settings() {
        let a: AgentConfig = serde_yaml::from_str(
            "compaction_threshold: 0.5\ncompaction_overhead_reserve: 0.25\n\
             compaction_keep_recent: 2\ncompaction_strategy: narrative\n",
        )
        .unwrap();
        let c = a.compaction();
        assert!((c.threshold - 0.5).abs() < f32::EPSILON);
        assert!((c.overhead_reserve - 0.25).abs() < f32::EPSILON);
        assert_eq!(c.keep_recent, 2);
        assert_eq!(c.strategy, CompactionStrategy::Narrative);
    }

    #[test]
    fn a_zero_child_run_timeout_means_no_limit() {
        let unbounded = AgentConfig {
            child_run_timeout_secs: 0,
            ..AgentConfig::default()
        };
        assert_eq!(unbounded.child_run_timeout(), None);
        assert_eq!(
            AgentConfig::default().child_run_timeout(),
            Some(Duration::from_secs(3600))
        );
    }
}
