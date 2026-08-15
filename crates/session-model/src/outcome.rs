// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Session outcome scoring: fold a `SessionEvent` stream into a reward.
//!
//! The reward is a per-session training weight consumed by an external
//! trainer, which reads exactly one JSON path off a serialized ATIF
//! trajectory: `final_metrics.extra.reward`. An absent, non-numeric or
//! non-finite value there means "outcome unknown" and the trajectory is
//! SKIPPED — never defaulted — so a half-finished session is simply left
//! unstamped rather than mis-scored.
//!
//! Two halves live here, both pure (no I/O, no clock):
//!
//! * [`OutcomeFold`] — counts the outcome-bearing events a surface already
//!   streams ([`SessionEvent::ToolCallFinished`], [`SessionEvent::Error`],
//!   [`SessionEvent::Aborted`], …).
//! * [`OutcomeFold::conclude`] — combines those counts with the
//!   [`RunConclusion`] the *surface* knows (its exit code, essentially) into
//!   a [`SessionReward`].
//!
//! Stamping the result onto a trajectory is deliberately NOT here: this crate
//! knows nothing about ATIF. See `sven_session_store::apply_reward_to_trajectory`.

use sven_vocab::SessionEvent;

/// How a run ended, as the surface driving it understands the ending.
///
/// This is the surface's verdict (typically derived from its exit code), not
/// the fold's — [`OutcomeFold::conclude`] may still veto a [`Self::Success`]
/// when the event stream disagrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunConclusion {
    /// The run finished its work and reported success.
    Success,
    /// The agent reported a fatal error.
    AgentError,
    /// The user (or a signal) cancelled the run.
    Cancelled,
    /// A run/step deadline elapsed.
    Timeout,
    /// The token budget was exhausted mid-run.
    BudgetExhausted,
}

/// Running tally of the outcome-bearing events seen during one session.
///
/// Deliberately does NOT descend into [`SessionEvent::SubagentEvent`]: the
/// reward scores *this* trajectory, and a subagent's own trajectory is scored
/// (or not) on its own terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutcomeFold {
    tool_calls: u32,
    tool_errors: u32,
    agent_errors: u32,
    aborted: bool,
    turns_completed: u32,
}

/// The stamped verdict for one session.
///
/// `reward` is guaranteed finite and within `0.0..=1.0` — a non-finite value
/// would serialize as JSON `null` and be read back as "no reward", silently
/// turning a scored session into a skipped one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionReward {
    /// Training weight in `[0.0, 1.0]`; `0.0` means "do not learn from this".
    pub reward: f64,
    /// Human-facing diagnostic label; never read programmatically.
    pub outcome: &'static str,
    /// Tool calls observed, for whoever is debugging a training run by eye.
    pub tool_calls: u32,
    /// Of those, how many came back with `is_error`.
    pub tool_errors: u32,
}

/// Weight of the tool-error ratio in a successful run's score: a run whose
/// every tool call failed still scores `1.0 - TOOL_ERROR_WEIGHT`, because it
/// did reach a successful conclusion.
const TOOL_ERROR_WEIGHT: f64 = 0.5;

impl OutcomeFold {
    /// Fold one event into the tally. Every variant that carries no outcome
    /// signal is a no-op — including subagent traffic, per the struct doc.
    ///
    /// The wildcard arm is deliberate: a new `SessionEvent` variant carries no
    /// outcome signal until someone decides it does, and silently not scoring
    /// it is the safe default.
    pub fn observe(&mut self, ev: &SessionEvent) {
        match ev {
            SessionEvent::ToolCallStarted(_) => self.tool_calls += 1,
            SessionEvent::ToolCallFinished { is_error, .. } => {
                if *is_error {
                    self.tool_errors += 1;
                }
            }
            SessionEvent::Error(_) => self.agent_errors += 1,
            SessionEvent::Aborted { .. } => self.aborted = true,
            SessionEvent::TurnComplete => self.turns_completed += 1,
            _ => {}
        }
    }

    /// Score the session, combining the surface's verdict with what the event
    /// stream actually showed.
    ///
    /// The stream can veto a [`RunConclusion::Success`] but never rescue a
    /// failure: a surface that reports success while having streamed an
    /// `Error` (or an `Aborted`, which `RuntimeRunner` reports as exit 0)
    /// scores zero.
    pub fn conclude(&self, conclusion: RunConclusion) -> SessionReward {
        let observed_failure = if self.agent_errors > 0 {
            Some("agent_error")
        } else if self.aborted {
            Some("cancelled")
        } else {
            None
        };

        let outcome = match conclusion {
            RunConclusion::AgentError => "agent_error",
            RunConclusion::Cancelled => "cancelled",
            RunConclusion::Timeout => "timeout",
            RunConclusion::BudgetExhausted => "budget_exhausted",
            RunConclusion::Success => match observed_failure {
                Some(label) => label,
                None if self.tool_errors > 0 => "success_with_tool_errors",
                None => "success",
            },
        };

        let succeeded = conclusion == RunConclusion::Success && observed_failure.is_none();
        let reward = if succeeded {
            1.0 - TOOL_ERROR_WEIGHT * self.tool_error_ratio()
        } else {
            0.0
        };
        // Belt and braces: a non-finite reward serializes as JSON `null`, which
        // reads back as "no reward" and silently turns a scored session into a
        // skipped one. `tool_error_ratio` already guards the division, so this
        // only ever fires if someone changes the formula above.
        let reward = if reward.is_finite() {
            reward.clamp(0.0, 1.0)
        } else {
            0.0
        };

        SessionReward {
            reward,
            outcome,
            tool_calls: self.tool_calls,
            tool_errors: self.tool_errors,
        }
    }

    /// Failed tool calls as a fraction of all of them, in `[0.0, 1.0]`.
    ///
    /// Errors can outnumber starts (a result whose `ToolCallStarted` was never
    /// observed), so the ratio is capped rather than assumed well-formed.
    fn tool_error_ratio(&self) -> f64 {
        if self.tool_calls == 0 {
            return if self.tool_errors > 0 { 1.0 } else { 0.0 };
        }
        (f64::from(self.tool_errors) / f64::from(self.tool_calls)).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::{SubagentUpdate, ToolCall};

    fn tool_call(id: &str) -> SessionEvent {
        SessionEvent::ToolCallStarted(ToolCall {
            id: id.to_string(),
            name: "read_file".to_string(),
            args: serde_json::json!({}),
        })
    }

    fn tool_result(id: &str, is_error: bool) -> SessionEvent {
        SessionEvent::ToolCallFinished {
            call_id: id.to_string(),
            tool_name: "read_file".to_string(),
            output: "out".to_string(),
            is_error,
        }
    }

    /// Fold `calls` tool calls of which `errors` failed.
    fn fold_with_tools(calls: u32, errors: u32) -> OutcomeFold {
        let mut f = OutcomeFold::default();
        for i in 0..calls {
            f.observe(&tool_call(&format!("c{i}")));
            f.observe(&tool_result(&format!("c{i}"), i < errors));
        }
        f
    }

    #[test]
    fn clean_success_scores_one() {
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::TurnComplete);
        let r = f.conclude(RunConclusion::Success);
        assert_eq!(r.reward, 1.0);
        assert_eq!(r.outcome, "success");
    }

    #[test]
    fn every_failure_conclusion_scores_zero() {
        // Even a fold full of perfectly successful tool calls must not rescue
        // a run the surface reports as failed.
        let f = fold_with_tools(5, 0);
        for (conclusion, label) in [
            (RunConclusion::AgentError, "agent_error"),
            (RunConclusion::Cancelled, "cancelled"),
            (RunConclusion::Timeout, "timeout"),
            (RunConclusion::BudgetExhausted, "budget_exhausted"),
        ] {
            let r = f.conclude(conclusion);
            assert_eq!(r.reward, 0.0, "{conclusion:?} must score 0.0");
            assert_eq!(r.outcome, label);
        }
    }

    #[test]
    fn tool_errors_grade_success_downward() {
        assert_eq!(fold_with_tools(10, 0).conclude(RunConclusion::Success).reward, 1.0);
        assert_eq!(fold_with_tools(10, 2).conclude(RunConclusion::Success).reward, 0.9);
        assert_eq!(fold_with_tools(10, 10).conclude(RunConclusion::Success).reward, 0.5);

        // Strictly monotone decreasing in the error count.
        let mut prev = f64::MAX;
        for errors in 0..=10 {
            let r = fold_with_tools(10, errors).conclude(RunConclusion::Success).reward;
            assert!(r < prev, "reward must decrease as errors grow: {r} !< {prev}");
            prev = r;
        }

        // Any tool error relabels the outcome, however small the penalty.
        assert_eq!(
            fold_with_tools(10, 1).conclude(RunConclusion::Success).outcome,
            "success_with_tool_errors"
        );
    }

    #[test]
    fn reward_is_always_finite_and_in_unit_interval() {
        // Load-bearing: a NaN/inf `f64` serializes as JSON `null`, which the
        // trainer reads as "no reward" and silently skips. `(0, 5)` is the
        // degenerate divide-by-zero shape (errors without a matching start).
        for conclusion in [
            RunConclusion::Success,
            RunConclusion::AgentError,
            RunConclusion::Cancelled,
            RunConclusion::Timeout,
            RunConclusion::BudgetExhausted,
        ] {
            for (calls, errors) in [(0, 0), (0, 5), (1, 0), (1, 1), (10, 3), (1000, 999)] {
                let mut f = fold_with_tools(calls, errors);
                if calls == 0 && errors > 0 {
                    // Results with no matching start: force the degenerate case.
                    for i in 0..errors {
                        f.observe(&tool_result(&format!("orphan{i}"), true));
                    }
                }
                let r = f.conclude(conclusion).reward;
                assert!(
                    r.is_finite() && (0.0..=1.0).contains(&r),
                    "{conclusion:?} with {calls}/{errors} produced {r}"
                );
            }
        }
    }

    #[test]
    fn agent_error_event_overrides_a_success_conclusion() {
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::Error("boom".to_string()));
        f.observe(&SessionEvent::TurnComplete);
        let r = f.conclude(RunConclusion::Success);
        assert_eq!(r.reward, 0.0);
        assert_eq!(r.outcome, "agent_error");
    }

    #[test]
    fn aborted_event_overrides_a_success_conclusion() {
        // `RuntimeRunner` returns EXIT_SUCCESS for an aborted run, so the fold
        // is the only thing that knows the run was actually cancelled.
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::Aborted {
            partial_text: "half".to_string(),
        });
        let r = f.conclude(RunConclusion::Success);
        assert_eq!(r.reward, 0.0);
        assert_eq!(r.outcome, "cancelled");
    }

    #[test]
    fn subagent_events_do_not_affect_the_parent_fold() {
        let mut f = OutcomeFold::default();
        let before = f.clone();
        f.observe(&SessionEvent::SubagentEvent {
            call_id: "c1".to_string(),
            handle_id: "h1".to_string(),
            update: SubagentUpdate::ToolCallFinished {
                id: "inner".to_string(),
                name: "shell".to_string(),
                output: "boom".to_string(),
                is_error: true,
            },
        });
        f.observe(&SessionEvent::SubagentEvent {
            call_id: "c1".to_string(),
            handle_id: "h1".to_string(),
            update: SubagentUpdate::Failed {
                reason: "nope".to_string(),
            },
        });
        assert_eq!(f, before, "subagent activity must not score the parent");
    }
}
