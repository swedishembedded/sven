// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// Swedish Embedded AB implements agents that keep working toward an
// objective until it is verifiably done for its clients. If your team needs
// expertise in supervising long-running agent work then you can procure our
// services by sending an email to info@swedishembedded.com.

//! Spec: a standing goal on the reactive agent.
//!
//! Set at rest, the goal starts a turn; every settled turn is followed by the
//! goal's check, its next turn or its end, and only its end brings the
//! machine to rest and ends the person's turn. The person's commands are
//! answered in any state, a cancelled or failed turn pauses the goal, and
//! nothing but the person continues a goal that stopped.

use serde_json::json;
use sven_hsm::{
    dispatch::Hsm, Context, Effect, Event, ProposedToolCall, ToolCallId, ToolCapability, UiEvent,
};
use sven_machines::machines::goal_flow::{self, UPDATE_GOAL_TOOL};
use sven_machines::machines::reactive_agent::{ReactiveState, CHAT_THREAD};
use sven_machines::ReactiveAgentMachine;
use sven_vocab::goal::{Goal, GoalCheck, GoalCommand, GoalStatus};
use sven_vocab::verify::{VerifierSpec, VerifierVerdict};

struct Session {
    hsm: Hsm<ReactiveAgentMachine>,
    ctx: Context,
}

impl Session {
    fn new() -> Self {
        let mut hsm = Hsm::new(ReactiveAgentMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        Self { hsm, ctx }
    }

    fn send(&mut self, event: Event) -> Vec<Effect> {
        self.hsm.dispatch(&event, &mut self.ctx).effects
    }

    /// A person's command, sent the way a surface sends it: waited for when
    /// nothing is running, otherwise only changing what follows.
    fn goal(&mut self, command: GoalCommand) -> Vec<Effect> {
        let awaits_answer = self.state() == ReactiveState::Idle;
        self.send(Event::Goal {
            command,
            awaits_answer,
        })
    }

    /// A command sent while an answer was open but dispatched once the
    /// machine came to rest: the race `formal/tla/Goal.tla` found.
    fn late(&mut self, command: GoalCommand) -> Vec<Effect> {
        self.send(Event::Goal {
            command,
            awaits_answer: false,
        })
    }

    fn state(&self) -> ReactiveState {
        self.hsm.state()
    }

    fn status(&self) -> Option<GoalStatus> {
        goal_flow::load(&self.ctx).map(|g| g.status)
    }

    /// The model answers without tool calls.
    fn reply(&mut self, text: &str) -> Vec<Effect> {
        self.send(Event::LlmTurnComplete {
            thread: CHAT_THREAD.to_string(),
            text: text.to_string(),
            tool_calls: vec![],
        })
    }

    /// The model calls one tool, which succeeds.
    fn call(&mut self, name: &str, args: serde_json::Value) -> Vec<Effect> {
        let call_id = ToolCallId::new();
        self.send(Event::LlmTurnComplete {
            thread: CHAT_THREAD.to_string(),
            text: String::new(),
            tool_calls: vec![ProposedToolCall {
                call_id,
                name: name.to_string(),
                args,
                capability: ToolCapability::ReadFile,
            }],
        });
        self.send(Event::ToolSucceeded {
            call_id,
            observation: json!("ok"),
        })
    }

    /// One goal turn that works and then answers.
    fn working_turn(&mut self) -> Vec<Effect> {
        self.call("read_file", json!({"path": "src/lib.rs"}));
        self.reply("made progress")
    }
}

fn set(objective: &str, max_turns: Option<u32>) -> GoalCommand {
    GoalCommand::Set {
        objective: objective.into(),
        max_turns,
        check: None,
    }
}

fn ends_turn(effects: &[Effect]) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Announce { event } if **event == UiEvent::TurnComplete))
}

fn goal_announced(effects: &[Effect]) -> Option<(Option<Goal>, String)> {
    effects.iter().find_map(|e| match e {
        Effect::Announce { event } => match event.as_ref() {
            UiEvent::GoalUpdated(update) => Some((update.goal.clone(), update.note.clone())),
            _ => None,
        },
        _ => None,
    })
}

fn turn_instruction(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::CallLlm { request } => {
            assert_eq!(request["machine_ends_turn"], json!(true), "{request}");
            Some(
                request["instruction"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        _ => None,
    })
}

fn check_command() -> GoalCommand {
    GoalCommand::Check {
        check: Some(GoalCheck {
            argv: vec!["cargo".into(), "test".into()],
            timeout_secs: 60,
        }),
    }
}

#[test]
fn setting_a_goal_starts_its_first_turn_and_offers_the_report_tool() {
    let mut s = Session::new();
    let effects = s.goal(set("make the parser tests pass", None));
    assert_eq!(s.state(), ReactiveState::Generating);
    let instruction = turn_instruction(&effects).expect("a goal turn");
    assert!(instruction.contains("make the parser tests pass"));
    let (goal, _) = goal_announced(&effects).expect("announced");
    assert_eq!(goal.unwrap().status, GoalStatus::Active);
    assert!(!ends_turn(&effects));
    assert!(s.ctx.active_tools.iter().any(|t| t == UPDATE_GOAL_TOOL));
}

#[test]
fn a_settled_turn_is_followed_by_the_next_without_ending_the_persons_turn() {
    let mut s = Session::new();
    s.goal(set("refactor the lexer", None));
    let effects = s.working_turn();
    assert_eq!(s.state(), ReactiveState::Generating);
    assert!(!ends_turn(&effects), "{effects:?}");
    let instruction = turn_instruction(&effects).expect("the next goal turn");
    assert!(instruction.contains("Goal turn 2 of 20"), "{instruction}");
}

#[test]
fn without_a_check_the_models_report_completes_it_unverified_and_ends_the_turn() {
    let mut s = Session::new();
    s.goal(set("write the changelog", None));
    s.call(
        UPDATE_GOAL_TOOL,
        json!({"status": "complete", "evidence": "CHANGELOG.md has the 4.5 section"}),
    );
    let effects = s.reply("Done.");
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.status, GoalStatus::Completed);
    assert!(!goal.verified);
}

#[test]
fn with_a_check_only_a_passing_check_completes_it() {
    let mut s = Session::new();
    s.goal(set("make the tests pass", None));
    // Set while the first turn runs: answered, the turn untouched.
    let answered = s.goal(check_command());
    assert!(goal_announced(&answered).is_some() && !ends_turn(&answered));

    s.call(
        UPDATE_GOAL_TOOL,
        json!({"status": "complete", "evidence": "I think so"}),
    );
    let effects = s.reply("All done!");
    assert_eq!(s.state(), ReactiveState::Checking);
    assert!(
        effects.iter().any(|e| matches!(e,
        Effect::Verify { spec: VerifierSpec::Command { argv, .. } } if argv == &["cargo", "test"]))
    );
    assert!(!ends_turn(&effects));

    // The model's word is not enough: the check fails, the goal goes on.
    let effects = s.send(Event::VerificationComplete {
        verdict: VerifierVerdict::Failed {
            reason: "test lexer::eof ... FAILED".into(),
        },
    });
    assert_eq!(s.state(), ReactiveState::Generating);
    let instruction = turn_instruction(&effects).unwrap();
    assert!(instruction.contains("lexer::eof"), "{instruction}");

    s.working_turn();
    let effects = s.send(Event::VerificationComplete {
        verdict: VerifierVerdict::Passed,
    });
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.status, GoalStatus::Completed);
    assert!(goal.verified);
}

#[test]
fn a_check_that_cannot_run_pauses_the_goal() {
    let mut s = Session::new();
    s.goal(set("make the tests pass", None));
    s.goal(check_command());
    s.working_turn();
    let effects = s.send(Event::EffectFailed {
        kind: sven_hsm::EffectKind::Verify,
        error: "refused".into(),
    });
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    assert_eq!(s.status(), Some(GoalStatus::Paused));
}

#[test]
fn pausing_lets_the_running_turn_finish_and_starts_no_other() {
    let mut s = Session::new();
    s.goal(set("port the parser", None));
    let answered = s.goal(GoalCommand::Pause);
    assert_eq!(s.state(), ReactiveState::Generating, "the turn is not cut");
    assert!(!ends_turn(&answered));
    let effects = s.working_turn();
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    assert!(turn_instruction(&effects).is_none());

    // Resumed at rest: the next goal turn starts, with a fresh budget.
    let effects = s.goal(GoalCommand::Resume);
    assert_eq!(s.state(), ReactiveState::Generating);
    assert!(turn_instruction(&effects).is_some());
    assert_eq!(s.status(), Some(GoalStatus::Active));
}

#[test]
fn clearing_while_a_turn_runs_lets_it_finish_as_an_ordinary_turn() {
    let mut s = Session::new();
    s.goal(set("port the parser", None));
    s.goal(GoalCommand::Clear);
    let effects = s.working_turn();
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    assert_eq!(s.status(), None);
}

#[test]
fn a_cancelled_turn_pauses_the_goal() {
    let mut s = Session::new();
    s.goal(set("port the parser", None));
    let effects = s.send(Event::UserCancelled);
    assert_eq!(s.state(), ReactiveState::Idle);
    assert_eq!(s.status(), Some(GoalStatus::Paused));
    assert!(goal_announced(&effects).is_some());
    // The abort ends the answer; nothing announces a second end.
    assert!(!ends_turn(&effects));
}

#[test]
fn a_failed_turn_pauses_the_goal_and_ends_the_turn() {
    let mut s = Session::new();
    s.goal(set("port the parser", None));
    let effects = s.send(Event::LlmFailed {
        error: "HTTP 402: insufficient credit".into(),
    });
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.status, GoalStatus::Paused);
    assert!(goal.reason.unwrap().contains("402"));
}

#[test]
fn the_budget_stops_the_goal_and_resume_grants_another() {
    let mut s = Session::new();
    s.goal(set("port the parser", Some(2)));
    s.working_turn();
    let effects = s.working_turn();
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    assert_eq!(s.status(), Some(GoalStatus::BudgetExhausted));
    s.goal(GoalCommand::Resume);
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!((goal.status, goal.turns_used), (GoalStatus::Active, 0));
}

#[test]
fn the_same_blocker_three_turns_running_blocks_the_goal_with_the_report() {
    let mut s = Session::new();
    s.goal(set("deploy the staging build", None));
    let blocked = json!({
        "status": "blocked",
        "blocker": "Permission denied writing /srv/staging",
        "evidence": "EACCES",
        "request": "grant write access to /srv/staging",
        "resume_condition": "the directory is writable",
    });
    for turn in 1..=3 {
        s.call(UPDATE_GOAL_TOOL, blocked.clone());
        let effects = s.reply("I am blocked.");
        if turn < 3 {
            assert_eq!(s.state(), ReactiveState::Generating, "turn {turn}");
        } else {
            assert_eq!(s.state(), ReactiveState::Idle);
            assert!(ends_turn(&effects));
        }
    }
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.status, GoalStatus::Blocked);
    assert_eq!(
        goal.blocker.unwrap().request,
        "grant write access to /srv/staging"
    );
}

#[test]
fn commands_at_rest_end_the_persons_turn() {
    let mut s = Session::new();
    for command in [
        GoalCommand::Pause,
        GoalCommand::Resume,
        GoalCommand::Clear,
        check_command(),
    ] {
        let effects = s.goal(command);
        assert_eq!(s.state(), ReactiveState::Idle);
        assert!(ends_turn(&effects), "{effects:?}");
        assert!(goal_announced(&effects).is_some());
    }
}

#[test]
fn a_goal_cannot_be_set_while_a_turn_runs() {
    let mut s = Session::new();
    s.send(Event::user_message("look at the build"));
    let effects = s.goal(set("make it green", None));
    assert_eq!(s.state(), ReactiveState::Generating);
    assert_eq!(s.status(), None);
    assert!(goal_announced(&effects)
        .unwrap()
        .1
        .contains("turn was running"));
}

#[test]
fn a_restored_goal_that_was_active_comes_back_paused() {
    let mut s = Session::new();
    let goal = Goal::new("goal-7".into(), "port the parser".into(), 20, None);
    let effects = s.goal(GoalCommand::Restore {
        goal: Box::new(goal),
    });
    assert_eq!(s.state(), ReactiveState::Idle);
    assert_eq!(s.status(), Some(GoalStatus::Paused));
    assert!(goal_announced(&effects).is_some());
    assert!(!ends_turn(&effects), "a restore answers nobody");
}

#[test]
fn an_ordinary_turn_with_no_goal_is_unchanged() {
    let mut s = Session::new();
    s.send(Event::user_message("hi"));
    let effects = s.reply("Hello!");
    assert_eq!(s.state(), ReactiveState::Idle);
    assert_eq!(effects.len(), 1);
    assert!(ends_turn(&effects));
}

#[test]
fn a_command_sent_during_a_turn_that_arrives_after_it_starts_and_ends_nothing() {
    // The answer it was sent in is already over: a goal turn started now
    // would run with no answer open, and a turn end would close the next.
    let mut s = Session::new();
    let effects = s.late(set("port the parser", None));
    assert_eq!(s.state(), ReactiveState::Idle);
    assert_eq!(s.status(), None, "not set");
    assert!(!ends_turn(&effects));

    s.goal(set("port the parser", None));
    s.goal(GoalCommand::Pause);
    s.working_turn();
    assert_eq!(s.status(), Some(GoalStatus::Paused));
    let effects = s.late(GoalCommand::Resume);
    assert_eq!(s.state(), ReactiveState::Idle, "no turn starts");
    assert_eq!(s.status(), Some(GoalStatus::Paused));
    assert!(!ends_turn(&effects));
    assert!(goal_announced(&effects)
        .unwrap()
        .1
        .contains("Resume it again"));

    for command in [GoalCommand::Pause, GoalCommand::Clear] {
        assert!(!ends_turn(&s.late(command)));
    }
}

#[test]
fn the_goal_the_person_asks_the_model_to_word_starts_in_the_same_answer() {
    let mut s = Session::new();
    s.send(Event::user_message(
        "set the most appropriately worded goal for this session",
    ));
    s.call(
        goal_flow::SET_GOAL_TOOL,
        json!({"objective": "Every parser test passes", "check": ["cargo", "test"]}),
    );
    let effects = s.reply("I set the goal: every parser test passes.");
    assert_eq!(
        s.state(),
        ReactiveState::Generating,
        "the goal starts at once"
    );
    assert!(!ends_turn(&effects), "in the same answer");
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.objective, "Every parser test passes");
    assert_eq!(goal.check.unwrap().argv, ["cargo", "test"]);
    assert!(turn_instruction(&effects)
        .unwrap()
        .contains("Every parser test passes"));
}

#[test]
fn the_model_cannot_change_the_goal_it_is_working_toward() {
    let mut s = Session::new();
    s.goal(set("port the parser", None));
    s.call(
        goal_flow::SET_GOAL_TOOL,
        json!({"objective": "something easier"}),
    );
    let effects = s.reply("I changed the goal.");
    let goal = goal_flow::load(&s.ctx).unwrap();
    assert_eq!(goal.objective, "port the parser", "unchanged");
    let instruction = turn_instruction(&effects).expect("the goal goes on");
    assert!(instruction.contains("not applied"), "{instruction}");
}

#[test]
fn a_goal_set_in_one_turn_does_not_leak_into_the_next() {
    let mut s = Session::new();
    s.send(Event::user_message("hi"));
    s.call(goal_flow::SET_GOAL_TOOL, json!({"objective": ""}));
    s.reply("ok");
    assert_eq!(s.status(), None, "an empty objective sets nothing");
    s.send(Event::user_message("thanks"));
    let effects = s.reply("you're welcome");
    assert_eq!(s.state(), ReactiveState::Idle);
    assert!(ends_turn(&effects));
    assert_eq!(s.status(), None);
}
