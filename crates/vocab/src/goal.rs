// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervised, long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A standing goal: what a session keeps working toward, and how it stands.
//!
//! A person sets a goal (`/goal <objective>`); the session's machine then
//! takes turn after turn toward it until it is done, blocked, paused or out
//! of turns. This module only names the data - the record ([`Goal`]), what a
//! person can ask of it ([`GoalCommand`]), what the model may report about it
//! ([`GoalReport`]) and how a surface is told ([`GoalUpdate`]). Every decision
//! is the machine's (`sven_machines::machines::goal`).
//!
//! The model never decides that a goal is done or blocked. It may *report*
//! either; a report of completion is believed only when no check is set, and
//! then the goal is marked unverified. A blocker ends a goal only once the
//! same blocker has been reported on [`BLOCKED_AFTER`] consecutive turns.

use serde::{Deserialize, Serialize};

/// Goal turns a goal gets when the person names no budget.
pub const DEFAULT_MAX_TURNS: u32 = 20;
/// The most goal turns a person may grant at once.
pub const MAX_TURNS_LIMIT: u32 = 500;
/// The tool the model sets the goal with when the person asks it to
/// (`set_goal`). Named here because the machine that honours it and the
/// tool that offers it live in crates that cannot depend on each other.
pub const SET_GOAL_TOOL: &str = "set_goal";
/// The tool the model reports on the goal with (`update_goal`).
pub const UPDATE_GOAL_TOOL: &str = "update_goal";

/// Consecutive goal turns that must report the same blocker before the goal
/// is blocked. One report is often a step the model can recover from itself.
pub const BLOCKED_AFTER: u32 = 3;
/// Consecutive goal turns without any action (no tool call, no report) after
/// which the goal is blocked: nothing is moving it any more.
pub const IDLE_AFTER: u32 = 3;
/// How long a goal's check may run when the person names no limit, seconds.
pub const DEFAULT_CHECK_TIMEOUT_SECS: u64 = 600;
/// The longest an objective may be, in characters.
pub const MAX_OBJECTIVE_CHARS: usize = 4_000;
/// The longest a field of a report may be, in characters: enough for an
/// error message and a paragraph, not a log.
pub const MAX_REPORT_FIELD_CHARS: usize = 2_000;

/// Where a goal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    /// The session is working toward it: every turn that ends is followed by
    /// the next, until something below holds.
    Active,
    /// Stopped by the person, by a cancelled turn, by a failure the session
    /// could not recover from, or by a restart. `/goal resume` continues it.
    Paused,
    /// The same obstacle stopped every attempt; [`Goal::blocker`] says what
    /// is needed. `/goal resume` continues it once that is done.
    Blocked,
    /// Done: the check passed, or (with no check set) the model reported it
    /// done ([`Goal::verified`] says which).
    Completed,
    /// Its turns ran out first. `/goal resume` grants a fresh budget.
    BudgetExhausted,
}

impl GoalStatus {
    /// Whether the session is still working toward the goal.
    #[must_use]
    pub fn is_active(self) -> bool {
        self == Self::Active
    }

    /// Whether `/goal resume` can continue a goal in this status.
    #[must_use]
    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Paused | Self::Blocked | Self::BudgetExhausted)
    }

    /// The status in a few words, for a person.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::BudgetExhausted => "out of turns",
        }
    }
}

/// The command that decides whether a goal is done: run in the session's
/// project, it passes when it exits with status 0 within its time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCheck {
    /// The program and its arguments, run without a shell of its own.
    pub argv: Vec<String>,
    /// How long it may run, in seconds.
    pub timeout_secs: u64,
}

/// What the last run of a goal's check found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CheckResult {
    /// It exited with status 0.
    Passed,
    /// It ran and did not pass; `detail` is the end of what it printed.
    Failed {
        /// Why, for the model and the person.
        detail: String,
    },
    /// No verdict: it could not be started, or nothing could run it.
    Unknown {
        /// Why, for the person.
        detail: String,
    },
}

/// What the person must know to unblock a goal, and what was done before.
///
/// The fields a person needs to act and the session needs to resume: what
/// failed and why, what it rests on, what is done and what is not, what is
/// asked of them, and what has to be true before work goes on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerReport {
    /// The operation that cannot be done and the obstacle.
    pub blocker: String,
    /// The error messages and results that show it.
    #[serde(default)]
    pub evidence: String,
    /// What has been done toward the goal so far.
    #[serde(default)]
    pub completed: String,
    /// What is left to do.
    #[serde(default)]
    pub remaining: String,
    /// What was checked, what passed, what failed, what is unknown.
    #[serde(default)]
    pub verification: String,
    /// What the person (or someone else) has to do.
    #[serde(default)]
    pub request: String,
    /// What has to be true before the work can go on.
    #[serde(default)]
    pub resume_condition: String,
}

/// What the model reported about the goal with the `update_goal` tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GoalReport {
    /// It believes every requirement is met; `evidence` says how it knows.
    Complete {
        /// What shows that the goal is met.
        evidence: String,
    },
    /// It cannot go on without outside help.
    Blocked(BlockerReport),
}

/// A standing goal and how far the session has got with it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    /// Names this goal, unlike any goal set before it in the session.
    pub id: String,
    /// What the person asked for, as they wrote it.
    pub objective: String,
    /// Where it stands.
    pub status: GoalStatus,
    /// The command that decides whether it is done, if the person set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<GoalCheck>,
    /// The last result of the check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<CheckResult>,
    /// Goal turns taken since it was set or last resumed.
    pub turns_used: u32,
    /// Goal turns it may take before it stops as out of turns.
    pub max_turns: u32,
    /// Completed because its check passed, not on the model's word.
    #[serde(default)]
    pub verified: bool,
    /// Why it is blocked, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<BlockerReport>,
    /// Why it stopped or what changed last, for the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Consecutive goal turns that reported the blocker in
    /// [`Goal::blocker_key`].
    #[serde(default)]
    pub blocked_turns: u32,
    /// The normalised blocker the consecutive reports were about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker_key: Option<String>,
    /// Consecutive goal turns that took no action at all.
    #[serde(default)]
    pub idle_turns: u32,
}

impl Goal {
    /// A goal the session starts working toward now.
    #[must_use]
    pub fn new(id: String, objective: String, max_turns: u32, check: Option<GoalCheck>) -> Self {
        Self {
            id,
            objective,
            status: GoalStatus::Active,
            check,
            last_check: None,
            turns_used: 0,
            max_turns,
            verified: false,
            blocker: None,
            reason: None,
            blocked_turns: 0,
            blocker_key: None,
            idle_turns: 0,
        }
    }

    /// Goal turns left before it runs out.
    #[must_use]
    pub fn turns_left(&self) -> u32 {
        self.max_turns.saturating_sub(self.turns_used)
    }
}

/// What a person asks of the session's goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum GoalCommand {
    /// Work toward `objective` from now on, replacing any goal there was.
    Set {
        /// What to achieve.
        objective: String,
        /// The goal turns it gets; [`DEFAULT_MAX_TURNS`] when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_turns: Option<u32>,
        /// The command that decides it, if completion is not to rest on the
        /// model's report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check: Option<GoalCheck>,
    },
    /// Stop taking further goal turns; the turn running finishes.
    Pause,
    /// Continue a paused, blocked or out-of-turns goal, with a fresh budget.
    Resume,
    /// Drop the goal.
    Clear,
    /// Decide completion with this command from now on, or (`None`) on the
    /// model's report again.
    Check {
        /// The command, or `None` to remove it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        check: Option<GoalCheck>,
    },
    /// Put back the goal a surface kept for this session, in a session it is
    /// opening again. A goal that was active comes back paused: the work it
    /// was doing may or may not have taken effect, so nothing is run again
    /// until the person says so.
    Restore {
        /// The goal as it was kept.
        goal: Box<Goal>,
    },
}

/// How a session's goal changed, for the surfaces: sent when a goal is set,
/// changes status, takes a turn, is checked or is cleared, and in answer to
/// every goal command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalUpdate {
    /// The goal now; `None` when the session has none.
    pub goal: Option<Goal>,
    /// What happened, in a sentence, for the person.
    pub note: String,
}

/// `text` cut to at most `max` characters, at a character boundary, marked
/// when it was cut. Used to bound everything a model or a command puts in a
/// goal, which travels in every event about it.
#[must_use]
pub fn bounded(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((end, _)) => format!("{}...", &text[..end]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_goal_round_trips_and_older_records_without_counters_still_read() {
        let goal = Goal::new("goal-1".into(), "fix the tests".into(), 20, None);
        let text = serde_json::to_string(&goal).unwrap();
        assert_eq!(serde_json::from_str::<Goal>(&text).unwrap(), goal);
        let minimal =
            r#"{"id":"goal-1","objective":"x","status":"paused","turns_used":2,"max_turns":20}"#;
        let read: Goal = serde_json::from_str(minimal).unwrap();
        assert_eq!(read.status, GoalStatus::Paused);
        assert_eq!(read.idle_turns, 0);
    }

    #[test]
    fn only_a_stopped_goal_short_of_done_can_be_resumed() {
        assert!(GoalStatus::Paused.is_resumable());
        assert!(GoalStatus::Blocked.is_resumable());
        assert!(GoalStatus::BudgetExhausted.is_resumable());
        assert!(!GoalStatus::Active.is_resumable());
        assert!(!GoalStatus::Completed.is_resumable());
    }

    #[test]
    fn bounded_cuts_at_a_character_and_marks_the_cut() {
        assert_eq!(bounded("abc", 5), "abc");
        assert_eq!(bounded("ååååå", 2), "åå...");
    }
}
