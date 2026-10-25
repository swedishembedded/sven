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
use sven_turn::{StallLimits, ThinkingBudget, TurnLimits};
use sven_vocab::AgentMode;

/// Default total wall-clock bound of a headless run, in seconds.
pub const DEFAULT_MAX_RUN_TIMEOUT_SECS: u64 = 4 * 60 * 60;

fn default_max_run_timeout_secs() -> u64 {
    DEFAULT_MAX_RUN_TIMEOUT_SECS
}

fn default_agent_mode() -> AgentMode {
    AgentMode::Agent
}
fn default_max_tool_rounds() -> u32 {
    200
}
fn default_compaction_threshold() -> f32 {
    // A conversation that still fits is worth keeping whole: compaction costs a
    // summary write and a loss of detail, and the output reserve below already
    // keeps room for the model's answer at the top of the window. On a window
    // small enough that the notes would not fit beside the conversation, the
    // request-fit bound fires earlier than this fraction does.
    0.98
}
fn default_child_run_timeout_secs() -> u64 {
    3600
}
fn default_compaction_summary_tokens() -> u32 {
    10_000
}
fn default_note_repeated_results() -> bool {
    true
}
fn default_tool_result_token_cap() -> usize {
    4000
}
fn default_compaction_overhead_reserve() -> f32 {
    0.0
}
fn default_compaction_output_reserve() -> f32 {
    // A turn near the trigger still needs room to think and answer, but the
    // threshold already leaves what the window has left, and sven asks for no
    // more output than the request leaves: a quarter of the window was reserved
    // twice over, and it made every episode compact at 60 % of its window.
    0.02
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
    /// checked by `TurnExecutor` before every model request against the usable
    /// input budget (`sven_model::budget::effective_input_budget`), minus
    /// `compaction_overhead_reserve`. Compaction also fires earlier when
    /// the request that asks for the notes would stop fitting the window.
    /// 1.0 turns compaction off: a run that must keep its whole transcript
    /// then ends at the budget gate instead of being rewritten.
    #[serde(default = "default_compaction_threshold")]
    pub compaction_threshold: f32,
    /// Most tokens the notes a compaction asks the model to write may take.
    /// Also capped at a quarter of the context window, so that the
    /// conversation being compacted and its notes fit together. The notes
    /// replace the conversation, so a larger value keeps more detail and
    /// leaves less room before the next compaction.
    #[serde(default = "default_compaction_summary_tokens")]
    pub compaction_summary_tokens: u32,
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
    /// Append a note to the result of a tool call that repeats an earlier
    /// call of the same run and gets the same result, telling the model the
    /// result is unchanged and that it must take a different action. Calls
    /// are compared without cosmetic differences (a `description` field,
    /// whitespace, key order).
    #[serde(default = "default_note_repeated_results")]
    pub note_repeated_results: bool,
    /// Fraction of the input budget set aside for what the token count does
    /// not see (a dynamic context block, estimation error). Reduces the
    /// effective compaction trigger threshold. The count is the server's own
    /// for everything already sent, so the default is 0.
    ///
    /// Example: threshold=0.95, reserve=0.05 → compaction fires when the
    /// request reaches 90% of the input budget (less the output reserve).
    #[serde(default = "default_compaction_overhead_reserve")]
    pub compaction_overhead_reserve: f32,
    /// Fraction of the context window kept free for the model's response,
    /// reasoning included. Compaction fires when the request plus this reserve
    /// would pass the threshold, so a normal turn always has room to think
    /// and answer: compaction, not a thinking cap, keeps the window in bounds.
    #[serde(default = "default_compaction_output_reserve")]
    pub compaction_output_reserve: f32,
    /// System prompt override; leave None to use the built-in prompt
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Per-step wall-clock timeout in seconds (0 = no limit).
    /// Can be set in config, overridden by frontmatter or CLI flag.
    #[serde(default)]
    pub max_step_timeout_secs: u64,

    /// Total run wall-clock timeout in seconds. The default
    /// (`DEFAULT_MAX_RUN_TIMEOUT_SECS`, four hours) is generous but finite,
    /// so an unattended run whose model, tool or approval handler never
    /// answers still ends; `0` = no limit. `--run-timeout` overrides it.
    #[serde(default = "default_max_run_timeout_secs")]
    pub max_run_timeout_secs: u64,

    /// Opt-in cap on estimated thinking/reasoning tokens for a single turn
    /// (chars/4 of accumulated `ThinkingDelta` content). `None` (the default)
    /// sets no cap: thinking is part of the response and is bounded by the
    /// request's output budget, the window less the prompt. A model that
    /// loops in its reasoning is stopped by a repetition detector and by
    /// `thinking_timeout_secs`, not by a token count. Live-adjustable in a
    /// session via `/think-limit`.
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

    /// Longest wait, in seconds, for a model server's response headers after
    /// the request is sent. `None` (the default) is 120s for a hosted API
    /// and 600s for a server on the user's own hardware (`brain`, `ollama`,
    /// `vllm`, `lmstudio`, `sven`), which may still be prefilling a long
    /// prompt. `0` is the default too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_timeout_secs: Option<u64>,

    /// Longest wait, in seconds from the request, for the model's first real
    /// output (text, reasoning or a tool-call chunk). Keepalives do not
    /// count, so a server that holds the connection open and produces
    /// nothing is cut off. `None` (the default) is 300s for a hosted API and
    /// 900s for a local server; never shorter than the first-byte limit.
    /// The stream-idle limit still applies to a server that sends nothing at
    /// all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_timeout_secs: Option<u64>,

    /// Longest silence, in seconds, of real output once the model has started
    /// producing it, however many keepalives arrive in between. `None` (the
    /// default) is 600s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_progress_timeout_secs: Option<u64>,

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
            "compaction_summary_tokens",
            "tool_result_token_cap",
            "note_repeated_results",
            "compaction_overhead_reserve",
            "compaction_output_reserve",
            "system_prompt",
            "max_step_timeout_secs",
            "max_run_timeout_secs",
            "max_thinking_tokens",
            "thinking_timeout_secs",
            "stream_idle_timeout_secs",
            "first_byte_timeout_secs",
            "first_token_timeout_secs",
            "no_progress_timeout_secs",
            "child_run_timeout_secs",
        ])
        .with(
            "compaction_keep_recent",
            Schema::ignored(
                "a compaction now summarises the whole conversation; remove the key",
            ),
        )
        .with(
            "compaction_strategy",
            Schema::ignored(
                "a compaction now asks for one set of notes on the whole conversation; remove the key",
            ),
        )
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
        .with_stall_limits(StallLimits::from_secs(
            self.first_byte_timeout_secs,
            self.first_token_timeout_secs,
            self.no_progress_timeout_secs,
        ))
    }

    /// When and how a session compacts its history.
    #[must_use]
    pub fn compaction(&self) -> CompactionConfig {
        CompactionConfig {
            threshold: self.compaction_threshold,
            overhead_reserve: self.compaction_overhead_reserve,
            output_reserve: self.compaction_output_reserve,
            summary_tokens: self.compaction_summary_tokens,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            default_mode: AgentMode::Agent,
            max_tool_rounds: 200,
            compaction_threshold: default_compaction_threshold(),
            compaction_summary_tokens: default_compaction_summary_tokens(),
            tool_result_token_cap: default_tool_result_token_cap(),
            note_repeated_results: default_note_repeated_results(),
            compaction_overhead_reserve: default_compaction_overhead_reserve(),
            compaction_output_reserve: default_compaction_output_reserve(),
            system_prompt: None,
            max_step_timeout_secs: 0,
            max_run_timeout_secs: DEFAULT_MAX_RUN_TIMEOUT_SECS,
            child_run_timeout_secs: default_child_run_timeout_secs(),
            max_thinking_tokens: None,
            thinking_timeout_secs: None,
            stream_idle_timeout_secs: None,
            first_byte_timeout_secs: None,
            first_token_timeout_secs: None,
            no_progress_timeout_secs: None,
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
        assert!((a.compaction_threshold - 0.98).abs() < f32::EPSILON);
        assert_eq!(a.compaction_summary_tokens, 10_000);
        assert!(a.system_prompt.is_none());
    }

    #[test]
    fn a_partial_section_keeps_the_defaults_for_absent_keys() {
        let a: AgentConfig = serde_yaml::from_str("compaction_summary_tokens: 6000\n").unwrap();
        assert_eq!(a.compaction_summary_tokens, 6000);
        assert_eq!(a.max_tool_rounds, 200);
    }

    #[test]
    fn the_mode_is_written_in_lowercase() {
        let a: AgentConfig = serde_yaml::from_str("default_mode: plan\n").unwrap();
        assert_eq!(a.default_mode, AgentMode::Plan);
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
        assert_eq!(limits.stream_idle, Some(Duration::from_secs(7)));
    }

    #[test]
    fn the_stall_keys_reach_the_turn_limits() {
        let a: AgentConfig = serde_yaml::from_str(
            "first_byte_timeout_secs: 30\nfirst_token_timeout_secs: 90\nno_progress_timeout_secs: 45\n",
        )
        .unwrap();
        assert_eq!(
            a.turn_limits().stall,
            StallLimits::from_secs(Some(30), Some(90), Some(45))
        );
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
             compaction_summary_tokens: 2000\n",
        )
        .unwrap();
        let c = a.compaction();
        assert!((c.threshold - 0.5).abs() < f32::EPSILON);
        assert!((c.overhead_reserve - 0.25).abs() < f32::EPSILON);
        assert_eq!(c.summary_tokens, 2000);
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

    #[test]
    fn the_retired_compaction_keys_still_load_and_are_reported_as_ignored() {
        let section = "compaction_keep_recent: 6\ncompaction_strategy: narrative\n\
                       compaction_summary_tokens: 5000\n";
        let agent: AgentConfig = serde_yaml::from_str(section).unwrap();
        assert_eq!(agent.compaction_summary_tokens, 5000);
        let document = sven_config::ConfigDocument::from_yaml(&format!(
            "agent:\n{}",
            section
                .lines()
                .map(|l| format!("  {l}\n"))
                .collect::<String>()
        ))
        .unwrap();
        let schema = sven_config::Schema::fields([("agent", AgentConfig::schema())]);
        let warnings = document.unknown_keys(&schema);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings.iter().all(|w| w.contains("is ignored")),
            "{warnings:?}"
        );
    }
}
