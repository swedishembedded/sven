// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervised, long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A session's standing goal in words, the same on every surface: what
//! `/goal` alone shows.

use sven_vocab::goal::{CheckResult, Goal, GoalCommand, GoalStatus};

/// What a `/goal` line asks of a surface that reads it as a whole prompt
/// (a headless run, an editor's prompt).
#[derive(Debug, PartialEq)]
pub enum GoalLine {
    /// A command for the session's goal.
    Command(GoalCommand),
    /// `/goal` alone: say how the goal stands.
    Show,
    /// The line misuses `/goal`: say this.
    Usage(String),
}

/// `text` as a `/goal` line; `None` when it is not one (an ordinary prompt,
/// or a word that only starts with it).
#[must_use]
pub fn goal_line(text: &str) -> Option<GoalLine> {
    use crate::commands::{builtin::goal, parser::tokenise, ImmediateAction};
    let rest = text.trim_start().strip_prefix("/goal")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(match goal::parse(&tokenise(rest.trim())).immediate_action {
        Some(ImmediateAction::Goal(command)) => GoalLine::Command(command),
        Some(ImmediateAction::ShowGoal) => GoalLine::Show,
        Some(ImmediateAction::Notice { text }) => GoalLine::Usage(text),
        _ => GoalLine::Usage(goal::GOAL_USAGE.to_string()),
    })
}

/// What a surface says when the session has no goal.
pub const NO_GOAL: &str = "No goal is set. Set one using /goal command";

/// The goal, its status and what it waits for, as plain text.
#[must_use]
pub fn describe(goal: Option<&Goal>) -> String {
    let Some(goal) = goal else {
        return NO_GOAL.to_string();
    };
    let mut text = format!(
        "Goal ({}, turn {} of {}):\n{}\n",
        goal.status.label(),
        goal.turns_used,
        goal.max_turns,
        goal.objective
    );
    match &goal.check {
        Some(check) => {
            let last = match &goal.last_check {
                None => "not run yet".to_string(),
                Some(CheckResult::Passed) => "passes".to_string(),
                Some(CheckResult::Failed { detail }) => {
                    format!("fails: {}", detail.lines().last().unwrap_or_default())
                }
                Some(CheckResult::Unknown { detail }) => format!("no verdict: {detail}"),
            };
            text.push_str(&format!("Check: `{}` ({last})\n", check.argv.join(" ")));
        }
        None => text.push_str("Check: none; the agent's report decides\n"),
    }
    if goal.status == GoalStatus::Completed && !goal.verified {
        text.push_str("Completed on the agent's report, unverified\n");
    }
    if let Some(reason) = &goal.reason {
        text.push_str(&format!("Why: {reason}\n"));
    }
    if let Some(blocker) = &goal.blocker {
        text.push_str(&format!("Blocker: {}\n", blocker.blocker));
        for (label, value) in [
            ("Evidence", &blocker.evidence),
            ("Done", &blocker.completed),
            ("Left", &blocker.remaining),
            ("Needed", &blocker.request),
            ("Resume when", &blocker.resume_condition),
        ] {
            if !value.is_empty() {
                text.push_str(&format!("{label}: {value}\n"));
            }
        }
    }
    let next = match goal.status {
        GoalStatus::Active => "/goal pause stops it after the turn running",
        GoalStatus::Paused | GoalStatus::Blocked | GoalStatus::BudgetExhausted => {
            "/goal resume continues it; /goal clear drops it"
        }
        GoalStatus::Completed => "/goal <objective> sets the next one",
    };
    text.push_str(next);
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::goal::{BlockerReport, GoalCheck};

    #[test]
    fn a_goal_line_is_read_the_way_the_terminal_reads_it() {
        assert_eq!(goal_line("fix the tests"), None);
        assert_eq!(goal_line("/goalpost moved"), None);
        assert_eq!(goal_line("/goal"), Some(GoalLine::Show));
        assert_eq!(goal_line("  /goal pause"), Some(GoalLine::Command(GoalCommand::Pause)));
        assert!(matches!(
            goal_line("/goal --turns 5 port the parser"),
            Some(GoalLine::Command(GoalCommand::Set { max_turns: Some(5), .. }))
        ));
        assert!(matches!(goal_line("/goal check"), Some(GoalLine::Usage(_))));
    }

    #[test]
    fn no_goal_says_how_to_set_one() {
        assert_eq!(describe(None), NO_GOAL);
    }

    #[test]
    fn a_blocked_goal_shows_what_is_needed_to_resume() {
        let mut goal = Goal::new("goal-1".into(), "deploy staging".into(), 20, None);
        goal.status = GoalStatus::Blocked;
        goal.blocker = Some(BlockerReport {
            blocker: "Permission denied on /srv".into(),
            request: "grant write access".into(),
            resume_condition: "/srv is writable".into(),
            ..BlockerReport::default()
        });
        let text = describe(Some(&goal));
        assert!(text.contains("blocked"));
        assert!(text.contains("Needed: grant write access"));
        assert!(text.contains("/goal resume"));
    }

    #[test]
    fn a_checked_goal_names_its_check_and_last_result() {
        let mut goal = Goal::new(
            "goal-1".into(),
            "tests pass".into(),
            20,
            Some(GoalCheck {
                argv: vec!["cargo".into(), "test".into()],
                timeout_secs: 60,
            }),
        );
        goal.last_check = Some(CheckResult::Failed {
            detail: "running 3 tests\ntest result: FAILED".into(),
        });
        let text = describe(Some(&goal));
        assert!(
            text.contains("`cargo test` (fails: test result: FAILED)"),
            "{text}"
        );
    }
}
