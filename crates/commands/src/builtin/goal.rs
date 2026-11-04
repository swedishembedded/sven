// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervised, long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.
//! `/goal` - set and steer the session's standing goal.
//!
//! The command only parses: every form becomes a typed
//! [`ImmediateAction::Goal`] that the surface hands to the session's kernel
//! (`SessionController::goal`), or [`ImmediateAction::ShowGoal`] for the
//! surface to show the goal it last heard of. Nothing here is sent to the
//! model as a message, so `/goal pause` works whatever the model is doing.

use sven_vocab::goal::{GoalCheck, GoalCommand, DEFAULT_CHECK_TIMEOUT_SECS};

use crate::{CommandContext, CommandResult, CompletionItem, ImmediateAction, SlashCommand};

/// How to use `/goal`, shown when it is used wrongly.
pub const GOAL_USAGE: &str =
    "Usage:\n  /goal [--turns N] <objective>   set the goal and start working on it\n  \
/goal                           show the goal\n  /goal pause | resume | clear\n  \
/goal check [--timeout SECS] <command...>   decide completion with a command\n  \
/goal check off                 decide on the agent's report again";

pub struct GoalCommandHandler;

fn action(action: ImmediateAction) -> CommandResult {
    CommandResult {
        immediate_action: Some(action),
        ..Default::default()
    }
}

fn usage(why: &str) -> CommandResult {
    action(ImmediateAction::Notice {
        text: format!("{why}\n\n{GOAL_USAGE}"),
    })
}

/// `/goal` with `args`, as a typed action.
#[must_use]
pub fn parse(args: &[String]) -> CommandResult {
    let Some(first) = args.first() else {
        return action(ImmediateAction::ShowGoal);
    };
    let rest = &args[1..];
    let only = |command: GoalCommand| {
        if rest.is_empty() {
            action(ImmediateAction::Goal(command))
        } else {
            usage(&format!("/goal {first} takes no arguments."))
        }
    };
    match first.as_str() {
        "status" | "show" => only_show(rest),
        "pause" => only(GoalCommand::Pause),
        "resume" => only(GoalCommand::Resume),
        "clear" => only(GoalCommand::Clear),
        "check" => parse_check(rest),
        "--turns" => match rest.split_first() {
            Some((turns, objective)) => match turns.parse::<u32>() {
                Ok(n) if n > 0 => set(objective, Some(n)),
                _ => usage("--turns takes a positive number."),
            },
            None => usage("--turns takes a number."),
        },
        _ => set(args, None),
    }
}

fn only_show(rest: &[String]) -> CommandResult {
    if rest.is_empty() {
        action(ImmediateAction::ShowGoal)
    } else {
        usage("/goal status takes no arguments.")
    }
}

fn set(objective: &[String], max_turns: Option<u32>) -> CommandResult {
    let objective = objective.join(" ");
    if objective.trim().is_empty() {
        return usage("A goal needs an objective.");
    }
    action(ImmediateAction::Goal(GoalCommand::Set {
        objective,
        max_turns,
    }))
}

fn parse_check(rest: &[String]) -> CommandResult {
    match rest {
        [] => usage("/goal check needs a command, or off."),
        [flag] if flag == "--timeout" => usage("/goal check needs a command."),
        [off] if matches!(off.as_str(), "off" | "none" | "remove") => {
            action(ImmediateAction::Goal(GoalCommand::Check { check: None }))
        }
        [flag, secs, argv @ ..] if flag == "--timeout" => match secs.parse::<u64>() {
            Ok(timeout_secs) if timeout_secs > 0 && !argv.is_empty() => {
                check(argv.to_vec(), timeout_secs)
            }
            Ok(_) | Err(_) if argv.is_empty() => usage("/goal check needs a command."),
            _ => usage("--timeout takes a positive number of seconds."),
        },
        argv => check(argv.to_vec(), DEFAULT_CHECK_TIMEOUT_SECS),
    }
}

fn check(argv: Vec<String>, timeout_secs: u64) -> CommandResult {
    action(ImmediateAction::Goal(GoalCommand::Check {
        check: Some(GoalCheck { argv, timeout_secs }),
    }))
}

impl SlashCommand for GoalCommandHandler {
    fn name(&self) -> &str {
        "goal"
    }

    fn description(&self) -> &str {
        "Set a standing goal the agent keeps working toward (/goal <objective> | pause | resume | clear | check <command>)"
    }

    fn complete(
        &self,
        arg_index: usize,
        partial: &str,
        _ctx: &CommandContext,
    ) -> Vec<CompletionItem> {
        if arg_index != 0 {
            return vec![];
        }
        let candidates = [
            ("pause", "stop after the turn running"),
            ("resume", "continue a paused, blocked or out-of-turns goal"),
            ("clear", "drop the goal"),
            ("check", "decide completion with a command"),
            ("status", "show the goal"),
        ]
        .map(|(value, desc)| CompletionItem::with_desc(value, value, desc));
        crate::completion::filter_and_rank(candidates.to_vec(), partial)
    }

    fn execute(&self, args: Vec<String>) -> CommandResult {
        parse(&args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &str) -> Option<ImmediateAction> {
        let args = crate::parser::tokenise(line);
        GoalCommandHandler.execute(args).immediate_action
    }

    #[test]
    fn an_objective_sets_the_goal_and_nothing_goes_to_the_model() {
        let result = GoalCommandHandler.execute(crate::parser::tokenise("make the tests pass"));
        assert!(result.message_to_send.is_none());
        assert!(matches!(
            result.immediate_action,
            Some(ImmediateAction::Goal(GoalCommand::Set { ref objective, max_turns: None }))
                if objective == "make the tests pass"
        ));
        assert!(matches!(
            run("--turns 40 port the parser"),
            Some(ImmediateAction::Goal(GoalCommand::Set { ref objective, max_turns: Some(40) }))
                if objective == "port the parser"
        ));
    }

    #[test]
    fn the_steering_words_are_commands_not_objectives() {
        assert!(matches!(
            run("pause"),
            Some(ImmediateAction::Goal(GoalCommand::Pause))
        ));
        assert!(matches!(
            run("resume"),
            Some(ImmediateAction::Goal(GoalCommand::Resume))
        ));
        assert!(matches!(
            run("clear"),
            Some(ImmediateAction::Goal(GoalCommand::Clear))
        ));
        assert!(matches!(run(""), Some(ImmediateAction::ShowGoal)));
        assert!(matches!(run("status"), Some(ImmediateAction::ShowGoal)));
        assert!(matches!(
            run("pause now"),
            Some(ImmediateAction::Notice { .. })
        ));
    }

    #[test]
    fn a_check_is_an_argv_with_an_optional_timeout() {
        match run(r#"check cargo test -p "my crate""#) {
            Some(ImmediateAction::Goal(GoalCommand::Check { check: Some(check) })) => {
                assert_eq!(check.argv, ["cargo", "test", "-p", "my crate"]);
                assert_eq!(check.timeout_secs, DEFAULT_CHECK_TIMEOUT_SECS);
            }
            other => panic!("{other:?}"),
        }
        match run("check --timeout 120 make test") {
            Some(ImmediateAction::Goal(GoalCommand::Check { check: Some(check) })) => {
                assert_eq!((check.argv.len(), check.timeout_secs), (2, 120));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            run("check off"),
            Some(ImmediateAction::Goal(GoalCommand::Check { check: None }))
        ));
        assert!(matches!(run("check"), Some(ImmediateAction::Notice { .. })));
        assert!(matches!(
            run("check --timeout x make"),
            Some(ImmediateAction::Notice { .. })
        ));
    }
}
