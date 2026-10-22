// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What a subagent is doing, folded from the `SubagentUpdate`s it streams:
//! whether it still runs, the tool it is in, how many tools it ran, how long
//! it has taken and how many tokens it has used. A frontend's activity view
//! reads this instead of re-deriving it from the subagent's transcript.
//!
//! Time is passed in, never read, so the fold is pure and replayable.
//!
//! Swedish Embedded AB implements solutions for supervising parallel
//! autonomous agents for its clients. If your team needs expertise in agent
//! runtimes and their observability then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::time::{Duration, Instant};

use sven_vocab::SubagentUpdate;

/// Where a subagent is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubagentState {
    Running,
    Completed,
    Failed { reason: String },
}

/// A subagent's progress, as far as its updates tell.
#[derive(Debug, Clone)]
pub struct SubagentActivity {
    /// The id of the tool call that started it, which is how the parent's
    /// transcript refers to it.
    pub call_id: String,
    /// What it was asked to do, in a few words.
    pub description: String,
    pub state: SubagentState,
    /// The tool it is running right now.
    pub current_tool: Option<String>,
    /// Tools it started, and how many of those failed.
    pub tools_run: usize,
    pub tools_failed: usize,
    pub started: Instant,
    pub ended: Option<Instant>,
    /// Tokens read and written so far. Summed from each request's reported
    /// usage while it runs, then replaced by the totals it reports at the end.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl SubagentActivity {
    /// A subagent that tool call `call_id` started at `now`.
    pub fn started(
        call_id: impl Into<String>,
        description: impl Into<String>,
        now: Instant,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            description: description.into(),
            state: SubagentState::Running,
            current_tool: None,
            tools_run: 0,
            tools_failed: 0,
            started: now,
            ended: None,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }
    }

    /// Fold one update, received at `now`.
    pub fn apply(&mut self, update: &SubagentUpdate, now: Instant) {
        match update {
            SubagentUpdate::ToolCallStarted { name, .. } => {
                self.tools_run += 1;
                self.current_tool = Some(name.clone());
            }
            SubagentUpdate::ToolCallFinished { is_error, .. } => {
                self.tools_failed += usize::from(*is_error);
                self.current_tool = None;
            }
            SubagentUpdate::Finished { .. } => self.end(SubagentState::Completed, now),
            SubagentUpdate::Failed { reason } => self.end(
                SubagentState::Failed {
                    reason: reason.clone(),
                },
                now,
            ),
            SubagentUpdate::TokenUsage { cost_usd } => self.cost_usd += cost_usd,
            SubagentUpdate::RequestLedger(request) => {
                if let Some(usage) = &request.reported {
                    self.input_tokens += u64::from(usage.logical_prompt());
                    self.output_tokens += u64::from(usage.output);
                }
            }
            SubagentUpdate::TokensUsed {
                input_tokens,
                output_tokens,
            } => {
                self.input_tokens = *input_tokens;
                self.output_tokens = *output_tokens;
            }
            SubagentUpdate::TextDelta(_) | SubagentUpdate::ThinkingDelta(_) => {}
        }
    }

    fn end(&mut self, state: SubagentState, now: Instant) {
        // The first ending stands: a failure reported after completion (or
        // the reverse) does not rewrite how the subagent ended.
        if self.state == SubagentState::Running {
            self.state = state;
            self.ended = Some(now);
            self.current_tool = None;
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == SubagentState::Running
    }

    /// How long it ran, or has been running at `now`.
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.ended
            .unwrap_or(now)
            .saturating_duration_since(self.started)
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[cfg(test)]
mod tests {
    use sven_vocab::{ReportedUsage, RequestBreakdown};

    use super::*;

    fn started(name: &str) -> SubagentUpdate {
        SubagentUpdate::ToolCallStarted {
            id: name.into(),
            name: name.into(),
            args: serde_json::Value::Null,
        }
    }

    fn finished(name: &str, is_error: bool) -> SubagentUpdate {
        SubagentUpdate::ToolCallFinished {
            id: name.into(),
            name: name.into(),
            output: String::new(),
            is_error,
        }
    }

    fn request(input: u32, output: u32) -> SubagentUpdate {
        SubagentUpdate::RequestLedger(Box::new(RequestBreakdown {
            reported: Some(ReportedUsage {
                input,
                output,
                ..ReportedUsage::default()
            }),
            ..RequestBreakdown::default()
        }))
    }

    #[test]
    fn a_running_subagent_shows_its_tool_its_count_its_time_and_its_tokens() {
        let t0 = Instant::now();
        let mut a = SubagentActivity::started("c1", "review auth", t0);
        a.apply(&started("read_file"), t0);
        a.apply(&finished("read_file", false), t0);
        a.apply(&request(1_000, 200), t0);
        a.apply(&started("shell"), t0);

        assert!(a.is_running());
        assert_eq!(a.current_tool.as_deref(), Some("shell"));
        assert_eq!(a.tools_run, 2);
        assert_eq!(a.total_tokens(), 1_200);
        assert_eq!(
            a.elapsed(t0 + Duration::from_secs(42)),
            Duration::from_secs(42)
        );
    }

    #[test]
    fn the_end_freezes_the_clock_and_its_reported_totals_replace_the_running_sum() {
        let t0 = Instant::now();
        let mut a = SubagentActivity::started("c2", "tests", t0);
        a.apply(&started("shell"), t0);
        a.apply(&finished("shell", true), t0);
        a.apply(&request(500, 50), t0);
        let t1 = t0 + Duration::from_secs(74);
        a.apply(
            &SubagentUpdate::Finished {
                final_text: "done".into(),
            },
            t1,
        );
        a.apply(
            &SubagentUpdate::TokensUsed {
                input_tokens: 900,
                output_tokens: 90,
            },
            t1,
        );
        a.apply(
            &SubagentUpdate::Failed {
                reason: "late".into(),
            },
            t1 + Duration::from_secs(5),
        );

        assert_eq!(a.state, SubagentState::Completed, "the first ending stands");
        assert_eq!(a.tools_failed, 1);
        assert_eq!(a.total_tokens(), 990);
        assert_eq!(
            a.elapsed(t1 + Duration::from_secs(600)),
            Duration::from_secs(74)
        );
    }
}
