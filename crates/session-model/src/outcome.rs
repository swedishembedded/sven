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
//! A run the surface or the event stream itself observed to have failed
//! (an agent-error event, an abort, a non-success exit) is scored
//! confidently — it does not need corroborating - but a *claimed* success
//! proves nothing on its own: an agent that says "Done!" is not evidence the
//! task was actually done. So a claimed success is only ever scored when a
//! verifier's [`Verdict`] backs it up; absent one, [`OutcomeFold::conclude`]
//! returns [`SessionOutcome::Unknown`] rather than a mechanically-assumed
//! `1.0`. That is the actual fix here: the previous formula stamped a
//! confident reward for a claim nothing had checked.
//!
//! Three parts live here, all pure (no I/O, no clock):
//!
//! * [`OutcomeFold`] — counts the outcome-bearing events a surface already
//!   streams ([`SessionEvent::ToolCallFinished`], [`SessionEvent::Error`],
//!   [`SessionEvent::Aborted`], …).
//! * [`OutcomeFold::conclude`] — combines those counts, the [`RunConclusion`]
//!   the *surface* knows (its exit code, essentially), and an optional
//!   [`Verdict`] into a [`SessionOutcome`].
//! * [`SessionOutcome`] — either a real, trainable [`SessionReward`], or an
//!   honest [`SessionOutcome::Unknown`] admission that none is possible yet.
//!
//! Producing a [`Verdict`] (running an actual verifier) and stamping the
//! result onto a trajectory are deliberately NOT here: this crate knows
//! nothing about verification mechanics or ATIF. See
//! `sven_session_store::apply_outcome_to_trajectory` for the latter.

use sven_vocab::SessionEvent;

/// How a run ended, as the surface driving it understands the ending.
///
/// This is the surface's verdict (typically derived from its exit code), not
/// the fold's — [`OutcomeFold::conclude`] may still veto a [`Self::Success`]
/// when the event stream disagrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// The run is parked on a question for a human and resumes when it is
    /// answered. Not an ending: neither a success nor a failure.
    Waiting,
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

/// The result of concluding a session: either a real score, or an honest
/// admission that none is possible yet.
///
/// [`Self::Unknown`] is not a failure - it means the outcome cannot be
/// trusted either way. The wire contract already treats an absent reward
/// this way (see the module doc); this type makes that the only way to
/// express it, rather than a caller having to remember to skip stamping.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SessionOutcome {
    /// A real, trainable score.
    Scored(SessionReward),
    /// No reward should be stamped. `reason` is a human-facing diagnostic,
    /// never read programmatically - callers branch on the variant, not the
    /// string.
    Unknown { reason: &'static str },
}

/// What a verifier concluded about a run that otherwise claims success.
///
/// Deliberately minimal: this is the seam [`OutcomeFold::conclude`] scores
/// against, not the verifier's own vocabulary (spec shape, origin, per-leaf
/// detail) - that lives wherever the verifier itself is implemented and
/// collapses to one of these two before it ever reaches this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The verifier confirmed the claimed outcome.
    Passed,
    /// The verifier found the claimed outcome false.
    Failed,
}

/// Weight of the tool-error ratio in a verified-success run's score: a run
/// whose every tool call failed still scores `1.0 - TOOL_ERROR_WEIGHT`,
/// because it did reach a verified conclusion.
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

    /// Score the session, combining the surface's verdict, what the event
    /// stream actually showed, and - only when the first two amount to a
    /// claimed success - a verifier's [`Verdict`], if one ran.
    ///
    /// A failure the surface or the event stream itself observed is scored
    /// confidently: it does not need a verifier to know it failed, and the
    /// stream can veto a [`RunConclusion::Success`] but never rescue a
    /// failure (a surface that reports success while having streamed an
    /// `Error`, or an `Aborted`, which `RuntimeRunner` reports as exit 0,
    /// scores zero). A *claimed* success is the only case a verifier's
    /// absence turns into [`SessionOutcome::Unknown`] rather than a
    /// mechanically-assumed `1.0`: an agent that reports success proves
    /// nothing on its own, and stamping a confident reward for an unverified
    /// claim is exactly the defect this type exists to stop.
    pub fn conclude(&self, conclusion: RunConclusion, verdict: Option<Verdict>) -> SessionOutcome {
        let observed_failure = if self.agent_errors > 0 {
            Some("agent_error")
        } else if self.aborted {
            Some("cancelled")
        } else {
            None
        };

        let confident_failure = match conclusion {
            // Still pending: scoring it at all would be a guess.
            RunConclusion::Waiting => {
                return SessionOutcome::Unknown {
                    reason: "waiting_for_human",
                }
            }
            RunConclusion::AgentError => Some("agent_error"),
            RunConclusion::Cancelled => Some("cancelled"),
            RunConclusion::Timeout => Some("timeout"),
            RunConclusion::BudgetExhausted => Some("budget_exhausted"),
            RunConclusion::Success => observed_failure,
        };
        if let Some(outcome) = confident_failure {
            return SessionOutcome::Scored(SessionReward {
                reward: 0.0,
                outcome,
                tool_calls: self.tool_calls,
                tool_errors: self.tool_errors,
            });
        }

        // A claimed success with no observed failure - the only case that
        // needs a verifier's word before it can be trusted either way.
        match verdict {
            None => SessionOutcome::Unknown {
                reason: "no_verifier",
            },
            Some(Verdict::Failed) => SessionOutcome::Scored(SessionReward {
                reward: 0.0,
                outcome: "verification_failed",
                tool_calls: self.tool_calls,
                tool_errors: self.tool_errors,
            }),
            Some(Verdict::Passed) => {
                let reward = 1.0 - TOOL_ERROR_WEIGHT * self.tool_error_ratio();
                // Belt and braces: a non-finite reward serializes as JSON
                // `null`, which reads back as "no reward" and silently turns
                // a scored session into a skipped one. `tool_error_ratio`
                // already guards the division, so this only ever fires if
                // someone changes the formula above.
                let reward = if reward.is_finite() {
                    reward.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                SessionOutcome::Scored(SessionReward {
                    reward,
                    outcome: "verified_success",
                    tool_calls: self.tool_calls,
                    tool_errors: self.tool_errors,
                })
            }
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
    fn a_conclusion_is_stored_as_its_snake_case_name() {
        let json = serde_json::to_string(&RunConclusion::BudgetExhausted).unwrap();
        assert_eq!(json, "\"budget_exhausted\"");
        let back: RunConclusion = serde_json::from_str(&json).unwrap();
        assert_eq!(back, RunConclusion::BudgetExhausted);
    }

    #[test]
    fn a_waiting_run_is_unknown_even_after_errors() {
        let outcome = fold_with_tools(2, 2).conclude(RunConclusion::Waiting, None);
        assert_eq!(
            outcome,
            SessionOutcome::Unknown {
                reason: "waiting_for_human"
            }
        );
    }

    #[test]
    fn an_unverified_success_is_unknown_not_scored() {
        // The defect this whole rework fixes: a run that merely *claims*
        // success (no tool errors, no agent-error event) used to score a
        // confident 1.0 with nothing behind it - the exact shape of "opened
        // a browser, failed to download the file, said Done!". Absent a
        // verifier's verdict, the honest answer is "we don't know", not "it
        // worked".
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::TurnComplete);
        let outcome = f.conclude(RunConclusion::Success, None);
        assert_eq!(
            outcome,
            SessionOutcome::Unknown {
                reason: "no_verifier"
            }
        );
    }

    #[test]
    fn a_verifier_confirmed_success_scores_one() {
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::TurnComplete);
        let outcome = f.conclude(RunConclusion::Success, Some(Verdict::Passed));
        let SessionOutcome::Scored(r) = outcome else {
            panic!("expected Scored, got {outcome:?}");
        };
        assert_eq!(r.reward, 1.0);
        assert_eq!(r.outcome, "verified_success");
    }

    #[test]
    fn a_verifier_that_disagrees_with_a_claimed_success_scores_zero() {
        // The other half of the same defect: the surface says success, the
        // verifier says otherwise. The verifier must win.
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::TurnComplete);
        let outcome = f.conclude(RunConclusion::Success, Some(Verdict::Failed));
        let SessionOutcome::Scored(r) = outcome else {
            panic!("expected Scored, got {outcome:?}");
        };
        assert_eq!(r.reward, 0.0);
        assert_eq!(r.outcome, "verification_failed");
    }

    /// Unwraps a `Scored` outcome, panicking with the actual value otherwise.
    fn scored(outcome: SessionOutcome) -> SessionReward {
        match outcome {
            SessionOutcome::Scored(r) => r,
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    #[test]
    fn every_failure_conclusion_scores_zero_confidently_with_no_verdict_needed() {
        // A failure the surface itself observed does not need a verifier to
        // know it failed - only a *claimed* success is suspect. Even a fold
        // full of perfectly successful tool calls must not rescue a run the
        // surface reports as failed.
        let f = fold_with_tools(5, 0);
        for (conclusion, label) in [
            (RunConclusion::AgentError, "agent_error"),
            (RunConclusion::Cancelled, "cancelled"),
            (RunConclusion::Timeout, "timeout"),
            (RunConclusion::BudgetExhausted, "budget_exhausted"),
        ] {
            let r = scored(f.conclude(conclusion, None));
            assert_eq!(r.reward, 0.0, "{conclusion:?} must score 0.0");
            assert_eq!(r.outcome, label);
        }
    }

    #[test]
    fn tool_errors_grade_a_verified_success_downward() {
        let verified = |calls, errors| {
            scored(
                fold_with_tools(calls, errors)
                    .conclude(RunConclusion::Success, Some(Verdict::Passed)),
            )
            .reward
        };
        assert_eq!(verified(10, 0), 1.0);
        assert_eq!(verified(10, 2), 0.9);
        assert_eq!(verified(10, 10), 0.5);

        // Strictly monotone decreasing in the error count.
        let mut prev = f64::MAX;
        for errors in 0..=10 {
            let r = verified(10, errors);
            assert!(
                r < prev,
                "reward must decrease as errors grow: {r} !< {prev}"
            );
            prev = r;
        }

        // Any tool error relabels the outcome, however small the penalty -
        // but the verified label wins over the mechanical one, since
        // verification is now what earned the score at all.
        assert_eq!(
            scored(fold_with_tools(10, 1).conclude(RunConclusion::Success, Some(Verdict::Passed)))
                .outcome,
            "verified_success"
        );
    }

    #[test]
    fn reward_is_always_finite_and_in_unit_interval() {
        // Load-bearing: a NaN/inf `f64` serializes as JSON `null`, which the
        // trainer reads as "no reward" and silently skips. `(0, 5)` is the
        // degenerate divide-by-zero shape (errors without a matching start).
        // `Success` is tested with a verdict on both sides (a `None` verdict
        // never produces a `Scored` at all - see `an_unverified_success_is_unknown_not_scored`).
        for (conclusion, verdict) in [
            (RunConclusion::Success, Some(Verdict::Passed)),
            (RunConclusion::Success, Some(Verdict::Failed)),
            (RunConclusion::AgentError, None),
            (RunConclusion::Cancelled, None),
            (RunConclusion::Timeout, None),
            (RunConclusion::BudgetExhausted, None),
        ] {
            for (calls, errors) in [(0, 0), (0, 5), (1, 0), (1, 1), (10, 3), (1000, 999)] {
                let mut f = fold_with_tools(calls, errors);
                if calls == 0 && errors > 0 {
                    // Results with no matching start: force the degenerate case.
                    for i in 0..errors {
                        f.observe(&tool_result(&format!("orphan{i}"), true));
                    }
                }
                let r = scored(f.conclude(conclusion, verdict)).reward;
                assert!(
                    r.is_finite() && (0.0..=1.0).contains(&r),
                    "{conclusion:?}/{verdict:?} with {calls}/{errors} produced {r}"
                );
            }
        }
    }

    #[test]
    fn agent_error_event_overrides_a_success_conclusion_with_no_verdict_needed() {
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::Error("boom".to_string()));
        f.observe(&SessionEvent::TurnComplete);
        let r = scored(f.conclude(RunConclusion::Success, None));
        assert_eq!(r.reward, 0.0);
        assert_eq!(r.outcome, "agent_error");
    }

    #[test]
    fn aborted_event_overrides_a_success_conclusion_with_no_verdict_needed() {
        // `RuntimeRunner` returns EXIT_SUCCESS for an aborted run, so the fold
        // is the only thing that knows the run was actually cancelled.
        let mut f = OutcomeFold::default();
        f.observe(&SessionEvent::Aborted {
            partial_text: "half".to_string(),
        });
        let r = scored(f.conclude(RunConclusion::Success, None));
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
