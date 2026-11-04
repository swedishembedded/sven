// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervising long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! A machine's standing goal, kept in its [`Context`]: the person's goal
//! commands, what each goal turn did, and the announcements the surfaces see.
//!
//! The decisions are [`super::goal`]'s; this module keeps their input and
//! output in the context (so a snapshot resumes a goal and a replay
//! reproduces it) and turns them into effects. Every change of the goal is
//! announced as `GoalUpdated`; a command answered at rest also ends the
//! person's turn, since nothing else will.

use serde_json::{json, Value};
use sven_hsm::{context::Context, effect::Effect, event::Event, ProposedToolCall, UiEvent};
use sven_vocab::goal::{
    bounded, BlockerReport, Goal, GoalCheck, GoalCommand, GoalReport, GoalStatus, GoalUpdate,
    DEFAULT_MAX_TURNS, MAX_OBJECTIVE_CHARS, MAX_REPORT_FIELD_CHARS, MAX_TURNS_LIMIT,
};
use sven_vocab::verify::VerifierVerdict;

use super::goal::{self, after_check, after_turn, check_summary, Next, TurnRecord};
use super::loop_core::LoopState;

pub use sven_vocab::goal::{SET_GOAL_TOOL, UPDATE_GOAL_TOOL};
/// Context fact holding the [`Goal`], or `null` when there is none.
pub const GOAL_FACT: &str = "goal";
/// Context fact counting the goals set in this session, so each gets an id
/// of its own without anything random in a transition.
const REVISION_FACT: &str = "goal.revision";
/// Context fact holding the [`TurnRecord`] of the goal turn running.
const TURN_FACT: &str = "goal.turn";
/// Context fact holding the goal the model set in the person's turn
/// running (`set_goal`), applied when the turn ends.
const PROPOSED_FACT: &str = "goal.proposed";
/// Context fact: the model tried to set the goal during the goal's own turn.
const REFUSED_FACT: &str = "goal.refused_set";

/// The session's goal, if it has one.
#[must_use]
pub fn load(ctx: &Context) -> Option<Goal> {
    ctx.fact(GOAL_FACT)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

fn store(ctx: &mut Context, goal: Option<&Goal>) {
    let value = goal.map_or(Value::Null, |g| {
        serde_json::to_value(g).unwrap_or(Value::Null)
    });
    ctx.set_fact(GOAL_FACT, value);
}

/// Whether the session is working toward a goal now.
#[must_use]
pub fn is_active(ctx: &Context) -> bool {
    load(ctx).is_some_and(|g| g.status.is_active())
}

fn turn(ctx: &Context) -> TurnRecord {
    ctx.fact(TURN_FACT)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn store_turn(ctx: &mut Context, record: &TurnRecord) {
    ctx.set_fact(
        TURN_FACT,
        serde_json::to_value(record).unwrap_or(Value::Null),
    );
}

/// Starts the record of a new goal turn.
pub fn begin_turn(ctx: &mut Context) {
    store_turn(ctx, &TurnRecord::default());
}

/// The announcement of how the goal stands now.
#[must_use]
pub fn announce(goal: Option<&Goal>, note: impl Into<String>) -> Effect {
    Effect::Announce {
        event: Box::new(UiEvent::GoalUpdated(Box::new(GoalUpdate {
            goal: goal.cloned(),
            note: note.into(),
        }))),
    }
}

/// What the machine does with a goal command.
#[derive(Debug)]
pub enum Answer {
    /// Emit these effects and stay where it is. At rest they end the
    /// person's turn.
    Stay(Vec<Effect>),
    /// Emit `announce` and take a goal turn with `instruction`.
    Start {
        /// The goal's announcement.
        announce: Effect,
        /// The goal turn's instruction.
        instruction: String,
    },
}

/// Carries out `command`. `at_rest` says whether the machine is waiting for
/// the person, `awaits_answer` whether the person waits for an answer to
/// this command. Only both together start a goal turn; the answer ends the
/// person's turn when they wait for it, by itself or with the work started.
pub fn command(
    ctx: &mut Context,
    command: &GoalCommand,
    at_rest: bool,
    awaits_answer: bool,
) -> Answer {
    let current = load(ctx);
    let starts = at_rest && awaits_answer;
    let stay = |ctx: &Context, note: String| {
        let mut effects = vec![announce(load(ctx).as_ref(), note)];
        if awaits_answer {
            effects.push(Effect::Announce {
                event: Box::new(UiEvent::TurnComplete),
            });
        }
        Answer::Stay(effects)
    };
    match command {
        GoalCommand::Set {
            objective,
            max_turns,
            check,
        } => {
            let objective = objective.trim();
            if objective.is_empty() {
                return stay(
                    ctx,
                    "A goal needs an objective: /goal <what to achieve>.".into(),
                );
            }
            if !starts {
                return stay(
                    ctx,
                    "A turn was running; set the goal once it has finished, or stop it first."
                        .into(),
                );
            }
            let revision = ctx.fact(REVISION_FACT).and_then(Value::as_u64).unwrap_or(0) + 1;
            ctx.set_fact(REVISION_FACT, json!(revision));
            let max_turns = max_turns
                .unwrap_or(DEFAULT_MAX_TURNS)
                .clamp(1, MAX_TURNS_LIMIT);
            let check = check
                .as_ref()
                .filter(|c| c.argv.first().is_some_and(|p| !p.is_empty()))
                .map(|c| GoalCheck {
                    argv: c.argv.clone(),
                    timeout_secs: c.timeout_secs.max(1),
                });
            let goal = Goal::new(
                format!("goal-{revision}"),
                bounded(objective, MAX_OBJECTIVE_CHARS),
                max_turns,
                check,
            );
            store(ctx, Some(&goal));
            ctx.activate_tools(&[UPDATE_GOAL_TOOL.to_string()]);
            begin_turn(ctx);
            Answer::Start {
                announce: announce(Some(&goal), format!("Goal set ({max_turns} turns).")),
                instruction: goal::instruction(&goal, true, &[]),
            }
        }
        GoalCommand::Pause => match current {
            Some(mut goal) if goal.status.is_active() => {
                goal.status = GoalStatus::Paused;
                goal.reason = Some("paused by you".into());
                store(ctx, Some(&goal));
                let note = if at_rest {
                    "Goal paused. /goal resume continues it."
                } else {
                    "Goal paused: the turn running finishes, and no further turn starts. \
                     /goal resume continues it."
                };
                stay(ctx, note.into())
            }
            Some(goal) => stay(
                ctx,
                format!("The goal is {}; nothing to pause.", goal.status.label()),
            ),
            None => stay(ctx, "There is no goal to pause.".into()),
        },
        GoalCommand::Resume => match current {
            Some(mut goal) if goal.status.is_resumable() => {
                goal.status = GoalStatus::Active;
                goal.turns_used = 0;
                goal.blocked_turns = 0;
                goal.blocker_key = None;
                goal.blocker = None;
                goal.idle_turns = 0;
                goal.reason = None;
                if at_rest && !starts {
                    return stay(
                        ctx,
                        "The goal was not resumed: a turn was running when you asked. Resume it again."
                            .into(),
                    );
                }
                store(ctx, Some(&goal));
                ctx.activate_tools(&[UPDATE_GOAL_TOOL.to_string()]);
                if starts {
                    begin_turn(ctx);
                    Answer::Start {
                        announce: announce(
                            Some(&goal),
                            format!("Goal resumed ({} turns).", goal.max_turns),
                        ),
                        instruction: goal::instruction(&goal, false, &[]),
                    }
                } else {
                    stay(
                        ctx,
                        "Goal resumed; it continues after the turn running.".into(),
                    )
                }
            }
            Some(goal) => stay(
                ctx,
                format!("The goal is {}; nothing to resume.", goal.status.label()),
            ),
            None => stay(ctx, "There is no goal to resume.".into()),
        },
        GoalCommand::Clear => {
            store(ctx, None);
            match current {
                Some(_) => stay(ctx, "Goal cleared.".into()),
                None => stay(ctx, "There is no goal to clear.".into()),
            }
        }
        GoalCommand::Check { check } => match current {
            Some(mut goal) => {
                let note = match check {
                    Some(check) if check.argv.first().is_some_and(|p| !p.is_empty()) => {
                        goal.check = Some(GoalCheck {
                            argv: check.argv.clone(),
                            timeout_secs: check.timeout_secs.max(1),
                        });
                        format!(
                            "The goal is now done when `{}` passes.",
                            check.argv.join(" ")
                        )
                    }
                    Some(_) => {
                        return stay(
                            ctx,
                            "A check needs a command: /goal check <command>.".into(),
                        )
                    }
                    None => {
                        goal.check = None;
                        "The goal's check is removed; the agent's report decides again.".into()
                    }
                };
                goal.last_check = None;
                store(ctx, Some(&goal));
                stay(ctx, note)
            }
            None => stay(ctx, "Set a goal first: /goal <what to achieve>.".into()),
        },
        GoalCommand::Restore { goal } => {
            let mut goal = (**goal).clone();
            if goal.status.is_active() {
                goal.status = GoalStatus::Paused;
                goal.reason = Some("the session was restarted; /goal resume continues it".into());
            }
            store(ctx, Some(&goal));
            if goal.status.is_resumable() {
                ctx.activate_tools(&[UPDATE_GOAL_TOOL.to_string()]);
            }
            Answer::Stay(vec![announce(Some(&goal), "Goal restored.")])
        }
    }
}

/// Notes what the model did in a goal turn: a call to any tool but the
/// goal report is an action.
pub fn observe_calls(ctx: &mut Context, calls: &[ProposedToolCall]) {
    if !is_active(ctx) || !calls.iter().any(|c| c.name != UPDATE_GOAL_TOOL) {
        return;
    }
    let mut record = turn(ctx);
    record.acted = true;
    store_turn(ctx, &record);
}

/// A person's turn starts: nothing the model set in an earlier one carries
/// over.
pub fn begin_person_turn(ctx: &mut Context) {
    ctx.set_fact(PROPOSED_FACT, Value::Null);
}

/// The goal the model set in the person's turn that just ended, carried out
/// as the person's own `/goal` would be; `None` when it set none.
pub fn proposed(ctx: &mut Context) -> Option<Answer> {
    let value = ctx.fact(PROPOSED_FACT).cloned()?;
    ctx.set_fact(PROPOSED_FACT, Value::Null);
    let set: GoalCommand = serde_json::from_value(value).ok()?;
    Some(command(ctx, &set, true, true))
}

/// The `set_goal` arguments as the person's `/goal` command.
fn parse_proposal(args: &Value) -> Option<GoalCommand> {
    let objective = args.get("objective").and_then(Value::as_str)?.trim();
    if objective.is_empty() {
        return None;
    }
    let max_turns = args
        .get("max_turns")
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok());
    let check = args
        .get("check")
        .and_then(Value::as_array)
        .and_then(|argv| {
            argv.iter()
                .map(|a| a.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
        })
        .filter(|argv| argv.first().is_some_and(|p| !p.is_empty()))
        .map(|argv| GoalCheck {
            argv,
            timeout_secs: sven_vocab::goal::DEFAULT_CHECK_TIMEOUT_SECS,
        });
    Some(GoalCommand::Set {
        objective: bounded(objective, MAX_OBJECTIVE_CHARS),
        max_turns,
        check,
    })
}

/// Notes a goal call the model made that ran: a report (`update_goal`)
/// during a goal turn, or a goal it set (`set_goal`). The call is read as
/// proposed, so one the tool refused (missing fields) counts for nothing. A
/// goal set during the goal's own turn is refused: the model never changes
/// what it is working toward on its own.
pub fn observe_result(ctx: &mut Context, event: &Event) {
    let Event::ToolSucceeded { call_id, .. } = event else {
        return;
    };
    let ls = LoopState::load(ctx);
    let Some(call) = ls.proposed.get(call_id) else {
        return;
    };
    if call.name == SET_GOAL_TOOL {
        if is_active(ctx) {
            ctx.set_fact(REFUSED_FACT, json!(true));
        } else if let Some(set) = parse_proposal(&call.args) {
            ctx.set_fact(
                PROPOSED_FACT,
                serde_json::to_value(set).unwrap_or(Value::Null),
            );
        }
        return;
    }
    if call.name != UPDATE_GOAL_TOOL || !is_active(ctx) {
        return;
    }
    if let Some(report) = parse_report(&call.args) {
        let mut record = turn(ctx);
        record.report = Some(report);
        store_turn(ctx, &record);
    }
}

/// A report from the `update_goal` arguments, each field bounded.
#[must_use]
pub fn parse_report(args: &Value) -> Option<GoalReport> {
    let field = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(|s| bounded(s.trim(), MAX_REPORT_FIELD_CHARS))
            .unwrap_or_default()
    };
    match args.get("status").and_then(Value::as_str)? {
        "complete" => Some(GoalReport::Complete {
            evidence: field("evidence"),
        }),
        "blocked" => {
            let blocker = field("blocker");
            (!blocker.is_empty()).then(|| {
                GoalReport::Blocked(BlockerReport {
                    blocker,
                    evidence: field("evidence"),
                    completed: field("completed"),
                    remaining: field("remaining"),
                    verification: field("verification"),
                    request: field("request"),
                    resume_condition: field("resume_condition"),
                })
            })
        }
        _ => None,
    }
}

/// The next step after a goal turn settled, with its announcement; `None`
/// when no goal is active (the turn ends as any turn does).
pub fn settled(ctx: &mut Context) -> Option<(Effect, Next)> {
    let mut goal = load(ctx).filter(|g| g.status.is_active())?;
    let record = turn(ctx);
    let mut next = after_turn(&mut goal, &record);
    store(ctx, Some(&goal));
    if !matches!(next, Next::Check(_)) {
        say_refusal(ctx, &mut next);
    }
    let note = match &next {
        Next::Check(_) => format!("Turn {} done; running the check.", goal.turns_used),
        Next::Continue(_) => format!("Turn {} of {}.", goal.turns_used + 1, goal.max_turns),
        Next::Stop => stopped_note(&goal),
    };
    if matches!(next, Next::Continue(_)) {
        begin_turn(ctx);
    }
    Some((announce(Some(&goal), note), next))
}

/// The next step after the goal's check answered; `None` when the goal was
/// cleared meanwhile.
pub fn checked(ctx: &mut Context, verdict: &VerifierVerdict) -> Option<(Effect, Next)> {
    let mut goal = load(ctx)?;
    let record = turn(ctx);
    let mut next = after_check(&mut goal, verdict, &record);
    store(ctx, Some(&goal));
    say_refusal(ctx, &mut next);
    let note = match &next {
        Next::Continue(_) => format!(
            "{}; turn {} of {}.",
            capitalise(&check_summary(&goal)),
            goal.turns_used + 1,
            goal.max_turns
        ),
        _ => stopped_note(&goal),
    };
    if matches!(next, Next::Continue(_)) {
        begin_turn(ctx);
    }
    Some((announce(Some(&goal), note), next))
}

/// Tells the model, in the goal's next turn, that the goal it tried to set
/// during the last one was not: only the person changes a running goal.
fn say_refusal(ctx: &mut Context, next: &mut Next) {
    if ctx.fact(REFUSED_FACT).and_then(Value::as_bool) != Some(true) {
        return;
    }
    ctx.set_fact(REFUSED_FACT, json!(false));
    if let Next::Continue(instruction) = next {
        instruction.push_str(
            "\nYour set_goal call was not applied: only the person changes the goal while it \
             runs. Work toward the goal above.\n",
        );
    }
}

/// An active goal stops because its turn did not finish: the person stopped
/// it, or it failed beyond its retries. Returns the announcement, if there
/// was an active goal.
pub fn interrupted(ctx: &mut Context, why: &str) -> Option<Effect> {
    let mut goal = load(ctx).filter(|g| g.status.is_active())?;
    goal.status = GoalStatus::Paused;
    goal.reason = Some(bounded(why, 500));
    store(ctx, Some(&goal));
    Some(announce(Some(&goal), stopped_note(&goal)))
}

/// What a goal that is no longer active says, for the person.
fn stopped_note(goal: &Goal) -> String {
    let reason = goal.reason.as_deref().unwrap_or_default();
    match goal.status {
        GoalStatus::Completed if goal.verified => format!("Goal completed: {reason}."),
        GoalStatus::Completed => format!("Goal completed, unverified: {reason}."),
        GoalStatus::Blocked => format!("Goal blocked: {reason}. /goal resume once it is cleared."),
        GoalStatus::BudgetExhausted => {
            format!(
                "Goal stopped: {reason}. /goal resume grants another {}.",
                goal.max_turns
            )
        }
        GoalStatus::Paused => format!("Goal paused: {reason}. /goal resume continues it."),
        GoalStatus::Active => "Goal active.".to_string(),
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
