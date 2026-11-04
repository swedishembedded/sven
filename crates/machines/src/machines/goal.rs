// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervising long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! The goal policy: what a session does after each turn it took toward its
//! standing goal.
//!
//! Pure functions over a [`Goal`] - no context, no effects - so the policy is
//! tested, model-checked (`formal/tla/Goal.tla`) and proved
//! (`formal/lean/Sven/Goal.lean`) without an agent around it. The machine
//! that runs a goal (`goal_flow`, used by the reactive agent) calls
//! [`after_turn`] when a goal turn has settled and [`after_check`] when the
//! goal's check has answered, and does what the returned [`Next`] says.
//!
//! The rules, in the order they apply:
//!
//! 1. A goal with a check is checked after every turn. Only a passing check
//!    completes it ([`Goal::verified`]); a check that reaches no verdict
//!    pauses it - never a pass.
//! 2. A goal without a check completes when the model reports it complete,
//!    and is marked unverified.
//! 3. A blocker the model reports ends the goal only once the same blocker
//!    was reported on [`BLOCKED_AFTER`] consecutive turns: one report is
//!    often a step the model gets past itself, and one turn without the
//!    report starts the count again.
//! 4. [`IDLE_AFTER`] consecutive turns with no action at all block the goal
//!    with a report the supervisor writes itself: the model need not admit
//!    that it is stuck for the session to stop.
//! 5. Out of turns, it stops as such - never as done.
//! 6. Otherwise it continues, with what the last turn and check showed.

use sven_vocab::goal::{
    bounded, BlockerReport, CheckResult, Goal, GoalReport, GoalStatus, BLOCKED_AFTER, IDLE_AFTER,
};
use sven_vocab::verify::{VerifierSpec, VerifierVerdict};

/// What happened in one goal turn, as the machine observed it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TurnRecord {
    /// The model called a tool other than the goal report.
    #[serde(default)]
    pub acted: bool,
    /// The model's last report about the goal in this turn.
    #[serde(default)]
    pub report: Option<GoalReport>,
}

/// What the machine does next for the goal.
#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    /// Run the goal's check, then ask [`after_check`].
    Check(VerifierSpec),
    /// Take the next goal turn with this instruction.
    Continue(String),
    /// The goal is no longer active: come to rest.
    Stop,
}

/// The verifier a goal's check is run as.
#[must_use]
pub fn check_spec(goal: &Goal) -> Option<VerifierSpec> {
    goal.check.as_ref().map(|check| VerifierSpec::Command {
        argv: check.argv.clone(),
        timeout_secs: check.timeout_secs,
    })
}

/// Decides after a goal turn settled: counts the turn and what it showed,
/// then checks, stops or continues. Leaves `goal` alone unless it is active.
pub fn after_turn(goal: &mut Goal, turn: &TurnRecord) -> Next {
    if !goal.status.is_active() {
        return Next::Stop;
    }
    goal.turns_used = goal.turns_used.saturating_add(1);
    if turn.acted || turn.report.is_some() {
        goal.idle_turns = 0;
    } else {
        goal.idle_turns = goal.idle_turns.saturating_add(1);
    }
    match &turn.report {
        Some(GoalReport::Blocked(report)) => {
            let key = blocker_key(&report.blocker);
            if goal.blocker_key.as_deref() == Some(key.as_str()) {
                goal.blocked_turns = goal.blocked_turns.saturating_add(1);
            } else {
                goal.blocked_turns = 1;
                goal.blocker_key = Some(key);
            }
            goal.blocker = Some(report.clone());
        }
        _ => {
            goal.blocked_turns = 0;
            goal.blocker_key = None;
            goal.blocker = None;
        }
    }
    match check_spec(goal) {
        Some(spec) => Next::Check(spec),
        None => decide(goal, turn, Vec::new()),
    }
}

/// Decides after the goal's check answered for the turn in `turn`.
pub fn after_check(goal: &mut Goal, verdict: &VerifierVerdict, turn: &TurnRecord) -> Next {
    let result = match verdict {
        VerifierVerdict::Passed => CheckResult::Passed,
        VerifierVerdict::Failed { reason } => CheckResult::Failed {
            detail: bounded(reason, 2_000),
        },
        VerifierVerdict::Unknown { reason } => CheckResult::Unknown {
            detail: bounded(reason, 500),
        },
        VerifierVerdict::NeedsHuman { question, .. } => CheckResult::Unknown {
            detail: bounded(question, 500),
        },
    };
    goal.last_check = Some(result.clone());
    match result {
        // True whatever the goal's status: the work is done.
        CheckResult::Passed => {
            goal.status = GoalStatus::Completed;
            goal.verified = true;
            goal.blocker = None;
            goal.reason = Some("its check passes".to_string());
            Next::Stop
        }
        _ if !goal.status.is_active() => Next::Stop,
        CheckResult::Unknown { detail } => {
            goal.status = GoalStatus::Paused;
            goal.reason = Some(format!("its check reached no verdict: {detail}"));
            Next::Stop
        }
        CheckResult::Failed { detail } => {
            let mut notes = vec![format!(
                "The goal's check does not pass yet:\n```\n{detail}\n```"
            )];
            if matches!(turn.report, Some(GoalReport::Complete { .. })) {
                notes.push(
                    "You reported the goal complete, but its check decides, and it fails."
                        .to_string(),
                );
            }
            decide(goal, turn, notes)
        }
    }
}

/// Rules 2 to 6, once the check (if any) has not completed the goal.
fn decide(goal: &mut Goal, turn: &TurnRecord, mut notes: Vec<String>) -> Next {
    if goal.check.is_none() {
        if let Some(GoalReport::Complete { evidence }) = &turn.report {
            goal.status = GoalStatus::Completed;
            goal.verified = false;
            goal.reason = Some(format!(
                "reported complete (no check is set): {}",
                bounded(evidence, 500)
            ));
            return Next::Stop;
        }
    }
    if goal.blocked_turns >= BLOCKED_AFTER {
        goal.status = GoalStatus::Blocked;
        goal.reason = goal
            .blocker
            .as_ref()
            .map(|b| format!("blocked: {}", bounded(&b.blocker, 300)));
        return Next::Stop;
    }
    if goal.idle_turns >= IDLE_AFTER {
        goal.status = GoalStatus::Blocked;
        goal.blocker = Some(idle_report(goal));
        goal.reason = Some(format!("{IDLE_AFTER} turns in a row took no action"));
        return Next::Stop;
    }
    if goal.turns_used >= goal.max_turns {
        goal.status = GoalStatus::BudgetExhausted;
        goal.reason = Some(format!("its {} turns are used up", goal.max_turns));
        return Next::Stop;
    }
    if let Some(blocker) = goal.blocker.as_ref().filter(|_| goal.blocked_turns > 0) {
        notes.push(format!(
            "You reported this blocker {} of {BLOCKED_AFTER} times in a row: {}. Look for \
             another way first; if the same obstacle stops you again, report it again.",
            goal.blocked_turns,
            bounded(&blocker.blocker, 300)
        ));
    }
    if goal.idle_turns > 0 {
        notes.push(format!(
            "Your last turn took no action ({} of {IDLE_AFTER}). Act on the goal, or report \
             what blocks you.",
            goal.idle_turns
        ));
    }
    Next::Continue(instruction(goal, false, &notes))
}

/// The report the supervisor writes when the model stopped acting without
/// saying why.
fn idle_report(goal: &Goal) -> BlockerReport {
    BlockerReport {
        blocker: format!("{IDLE_AFTER} goal turns in a row took no action"),
        evidence: "the model answered without calling a tool or reporting on the goal".into(),
        completed: format!("{} goal turns taken", goal.turns_used),
        remaining: bounded(&goal.objective, 300),
        verification: check_summary(goal),
        request: "say how to proceed, change the goal, or set a check that decides it".into(),
        resume_condition: "the goal or its instructions say what to do next".into(),
    }
}

/// The last check result in a line.
#[must_use]
pub fn check_summary(goal: &Goal) -> String {
    match (&goal.check, &goal.last_check) {
        (None, _) => "no check is set".to_string(),
        (Some(_), None) => "the check has not run yet".to_string(),
        (Some(_), Some(CheckResult::Passed)) => "the check passes".to_string(),
        (Some(_), Some(CheckResult::Failed { .. })) => "the check fails".to_string(),
        (Some(_), Some(CheckResult::Unknown { detail })) => {
            format!("the check reached no verdict: {detail}")
        }
    }
}

/// A blocker reduced to what identifies it, so that the same obstacle in
/// slightly different words counts as the same: lower case, letters and
/// digits only, at most the first twelve words.
#[must_use]
pub fn blocker_key(blocker: &str) -> String {
    blocker
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(12)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The instruction of a goal turn: the goal as the person set it, how it
/// is decided, how to report, and what the last turn showed.
#[must_use]
pub fn instruction(goal: &Goal, opening: bool, notes: &[String]) -> String {
    let turn = goal.turns_used.saturating_add(1);
    let mut text = format!(
        "[Goal turn {turn} of {max}]\nThe standing goal the person set:\n<goal>\n{objective}\n</goal>\n\n{start}\n\
         - The workspace as it is now is your evidence: look at it rather than relying on what \
         you remember.\n\
         - Make concrete progress in this turn: change something, run something, or find out \
         what decides the next step. Restating the status, or a plan you do not carry out, is \
         not progress.\n",
        max = goal.max_turns,
        objective = goal.objective,
        start = if opening {
            "Start working toward it now; the session keeps giving you turns until it is met."
        } else {
            "Continue working toward it."
        },
    );
    match &goal.check {
        Some(check) => text.push_str(&format!(
            "- The goal is met when `{}` passes. It runs after every turn; you do not declare \
             the goal done.\n",
            check.argv.join(" ")
        )),
        None => text.push_str(
            "- When every requirement is met and you have checked it, call `update_goal` with \
             status \"complete\" and the evidence. Uncertain or indirect evidence does not count.\n",
        ),
    }
    text.push_str(
        "- If something outside your control stops you (a permission, a credential, a service, \
         a decision only the person can make), call `update_goal` with status \"blocked\": what \
         fails, the evidence, what is done, what remains, what you need, and what must be true \
         to resume. Never work around a permission or a security boundary.\n",
    );
    for note in notes {
        text.push('\n');
        text.push_str(note);
        text.push('\n');
    }
    if goal.turns_left() == 1 {
        text.push_str(
            "\nThis is the goal's last turn: finish if you can; otherwise leave the work so it \
             can be resumed and say what remains.\n",
        );
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_vocab::goal::GoalCheck;

    fn goal(max_turns: u32, check: bool) -> Goal {
        Goal::new(
            "goal-1".into(),
            "make the parser tests pass".into(),
            max_turns,
            check.then(|| GoalCheck {
                argv: vec!["cargo".into(), "test".into()],
                timeout_secs: 60,
            }),
        )
    }

    fn acted() -> TurnRecord {
        TurnRecord {
            acted: true,
            report: None,
        }
    }

    fn complete() -> TurnRecord {
        TurnRecord {
            acted: true,
            report: Some(GoalReport::Complete {
                evidence: "tests pass".into(),
            }),
        }
    }

    fn blocked(what: &str) -> TurnRecord {
        TurnRecord {
            acted: true,
            report: Some(GoalReport::Blocked(BlockerReport {
                blocker: what.into(),
                ..BlockerReport::default()
            })),
        }
    }

    #[test]
    fn with_a_check_the_models_word_never_completes_the_goal() {
        let mut g = goal(20, true);
        assert!(matches!(after_turn(&mut g, &complete()), Next::Check(_)));
        let failed = VerifierVerdict::Failed {
            reason: "1 test failed".into(),
        };
        let next = after_check(&mut g, &failed, &complete());
        assert!(matches!(next, Next::Continue(ref text) if text.contains("1 test failed")));
        assert_eq!(g.status, GoalStatus::Active);
    }

    #[test]
    fn a_passing_check_completes_the_goal_verified_whatever_the_model_said() {
        let mut g = goal(20, true);
        after_turn(&mut g, &acted());
        assert_eq!(
            after_check(&mut g, &VerifierVerdict::Passed, &acted()),
            Next::Stop
        );
        assert_eq!(g.status, GoalStatus::Completed);
        assert!(g.verified);
    }

    #[test]
    fn a_check_without_a_verdict_pauses_never_passes() {
        let mut g = goal(20, true);
        after_turn(&mut g, &complete());
        let unknown = VerifierVerdict::Unknown {
            reason: "cargo: not found".into(),
        };
        assert_eq!(after_check(&mut g, &unknown, &complete()), Next::Stop);
        assert_eq!(g.status, GoalStatus::Paused);
        assert!(!g.verified);
    }

    #[test]
    fn without_a_check_a_report_completes_the_goal_unverified() {
        let mut g = goal(20, false);
        assert_eq!(after_turn(&mut g, &complete()), Next::Stop);
        assert_eq!(g.status, GoalStatus::Completed);
        assert!(!g.verified);
    }

    #[test]
    fn a_blocker_blocks_only_when_reported_on_consecutive_turns() {
        let mut g = goal(20, false);
        assert!(matches!(
            after_turn(&mut g, &blocked("Permission denied: /etc/app.conf")),
            Next::Continue(_)
        ));
        assert!(matches!(
            after_turn(&mut g, &blocked("permission denied /etc/app.conf")),
            Next::Continue(_)
        ));
        assert_eq!(g.blocked_turns, 2, "the same blocker in other words");
        assert_eq!(
            after_turn(&mut g, &blocked("Permission denied: /etc/app.conf.")),
            Next::Stop
        );
        assert_eq!(g.status, GoalStatus::Blocked);
        assert!(g.blocker.is_some());

        // A turn without the report starts the count again.
        let mut g = goal(20, false);
        after_turn(&mut g, &blocked("no network"));
        after_turn(&mut g, &blocked("no network"));
        after_turn(&mut g, &acted());
        assert!(matches!(
            after_turn(&mut g, &blocked("no network")),
            Next::Continue(_)
        ));
        assert_eq!(g.blocked_turns, 1);
    }

    #[test]
    fn repeated_work_that_keeps_acting_is_not_stuck() {
        // The same failing test run after every edit is work, not a stall:
        // only the budget ends it.
        let mut g = goal(5, true);
        let failed = VerifierVerdict::Failed {
            reason: "test parse ... FAILED".into(),
        };
        for _ in 0..4 {
            after_turn(&mut g, &acted());
            assert!(matches!(
                after_check(&mut g, &failed, &acted()),
                Next::Continue(_)
            ));
        }
        after_turn(&mut g, &acted());
        assert_eq!(after_check(&mut g, &failed, &acted()), Next::Stop);
        assert_eq!(g.status, GoalStatus::BudgetExhausted);
    }

    #[test]
    fn turns_without_action_block_the_goal_with_the_supervisors_report() {
        let mut g = goal(20, false);
        let idle = TurnRecord::default();
        assert!(matches!(after_turn(&mut g, &idle), Next::Continue(ref t) if t.contains("1 of 3")));
        after_turn(&mut g, &idle);
        assert_eq!(after_turn(&mut g, &idle), Next::Stop);
        assert_eq!(g.status, GoalStatus::Blocked);
        let report = g.blocker.expect("the supervisor writes the report");
        assert!(!report.request.is_empty() && !report.resume_condition.is_empty());
    }

    #[test]
    fn the_budget_ends_the_goal_as_out_of_turns_never_as_done() {
        let mut g = goal(2, false);
        assert!(
            matches!(after_turn(&mut g, &acted()), Next::Continue(ref t) if t.contains("last turn"))
        );
        assert_eq!(after_turn(&mut g, &acted()), Next::Stop);
        assert_eq!(g.status, GoalStatus::BudgetExhausted);
        assert_eq!(g.turns_used, 2);
    }

    #[test]
    fn a_goal_that_is_not_active_takes_no_further_turn() {
        let mut g = goal(20, false);
        g.status = GoalStatus::Paused;
        assert_eq!(after_turn(&mut g, &acted()), Next::Stop);
        assert_eq!(g.turns_used, 0);
    }

    #[test]
    fn the_instruction_names_the_goal_and_how_it_is_decided() {
        let g = goal(20, true);
        let text = instruction(&g, true, &[]);
        assert!(text.contains("make the parser tests pass"));
        assert!(text.contains("`cargo test` passes"));
        assert!(text.contains("update_goal"));
        assert!(text.contains("Goal turn 1 of 20"));
    }
}
