// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! A one-shot child submachine that executes a single decomposed task.
//!
//! [`TaskMachine`] is the unit a parent [`SdlcMachine`](super::SdlcMachine) fans
//! out to during parallel execution: each instance runs **one** task on its own
//! isolated `task` conversation thread (a fresh [`Context`]), then completes.
//! Its terminal `out` fact carries the structured result the parent harvests and
//! reports up as `SubmachineCompleted`.
//!
//! # Kernel-native loop
//!
//! `TaskMachine` uses the same kernel-mediated pattern as the SDLC phases:
//! `Effect::CallLlm kind="turn"` → `LlmTurnComplete{text, tool_calls}` → tool
//! calls run in-state through [`handle_tool_event`] → continuation → the final
//! text is parsed as a decision.
//!
//! # Reaching a person
//!
//! A child has no person of its own to ask. When the run that spawned it can
//! reach one, the spawner says so with [`TaskMachine::HUMAN_GATE_FACT`], and a
//! decision of `need_user_input` or `need_approval` - like a tool call that
//! needs approval - is asked through `AskUser` / `RequestHumanApproval`; the
//! child waits for the answer and carries on. Without that fact the task
//! ends with `ok: false` and a summary that says a person was needed and none
//! could be reached.

use serde_json::{json, Value};
use sven_hsm::{
    context::Context,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    status::Reaction,
};

use super::decisions::{message_of, payload_of, status_of, DecisionStatus};
use super::{ask_user_effect, phase_continuation_turn, phase_followup_turn, prompts};
use crate::machines::loop_core::{
    handle_tool_event, init_loop, max_rounds, on_llm_turn_complete, GeneratingAction,
};
use crate::MAX_TOOL_ROUNDS_FACT;

/// Most tool-call rounds a task turn takes, however much the run allows.
const TASK_MAX_TOOL_ROUNDS: u32 = 40;

/// The fact recording that the task is waiting for a person's answer.
const AWAITING_ANSWER_FACT: &str = "task.awaiting_answer";

/// States of the one-shot task submachine.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TaskState {
    Top,
    /// The task's turn loop, including its tool calls and any wait for a
    /// person's answer.
    Run,
    Done,
}

/// A child submachine that implements a single task to completion.
pub struct TaskMachine {
    id: MachineId,
    task: String,
}

impl TaskMachine {
    /// Create a task submachine for `task`.
    #[must_use]
    pub fn new(task: impl Into<String>) -> Self {
        Self {
            id: MachineId::new(),
            task: task.into(),
        }
    }

    /// The fact key under which the terminal result is stored.
    pub const RESULT_FACT: &'static str = "out";

    /// Set to `true` by a spawner whose child can reach a person through the
    /// parent's question and approval channels. Absent or `false`, a task
    /// that needs a person ends instead of waiting.
    pub const HUMAN_GATE_FACT: &'static str = "task.human_gate";

    fn finish(
        &self,
        ctx: &mut Context,
        summary: String,
        ok: bool,
        payload: Value,
    ) -> Reaction<TaskState> {
        ctx.set_fact(
            Self::RESULT_FACT,
            json!({"task": self.task, "summary": summary, "ok": ok, "payload": payload}),
        );
        Reaction::transition(TaskState::Done, [], "task complete")
    }

    /// A follow-up turn carrying `instruction`, on the task's own thread.
    fn follow_up(ctx: &Context, instruction: &str) -> Reaction<TaskState> {
        let instruction = format!("{instruction} {}", prompts::ANSWER_CONTRACT);
        Reaction::effects(vec![phase_followup_turn(
            "task",
            prompts::WRITE_TOOLS,
            &instruction,
            max_rounds(ctx) as u32,
        )])
    }

    /// What the task does with its final decision.
    fn conclude(&self, ctx: &mut Context, decision: Value) -> Reaction<TaskState> {
        let status = status_of(&decision);
        let needs = match status {
            DecisionStatus::NeedUserInput => Some("an answer"),
            DecisionStatus::NeedApproval => Some("an approval"),
            _ => None,
        };
        match needs {
            Some(_) if human_gate(ctx) => match status {
                DecisionStatus::NeedUserInput => {
                    ctx.set_fact(AWAITING_ANSWER_FACT, true);
                    Reaction::effects(vec![ask_user_effect(&decision)])
                }
                _ => Reaction::effects(vec![super::request_approval(ctx, &decision)]),
            },
            Some(what) => {
                let summary = format!(
                    "the task needs {what} from a person, and this child run has no way \
                     to reach one: {}",
                    message_of(&decision)
                );
                self.finish(ctx, summary, false, payload_of(&decision))
            }
            None => {
                let ok = status == DecisionStatus::Proceed;
                self.finish(ctx, message_of(&decision), ok, payload_of(&decision))
            }
        }
    }
}

/// Whether the spawner connected this child to a person.
fn human_gate(ctx: &Context) -> bool {
    ctx.fact(TaskMachine::HUMAN_GATE_FACT)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// The task's round budget: its own cap, lowered by the run's when that is
/// smaller.
fn task_max_rounds(ctx: &Context) -> u32 {
    ctx.fact(MAX_TOOL_ROUNDS_FACT)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .and_then(|n| u32::try_from(n).ok())
        .map_or(TASK_MAX_TOOL_ROUNDS, |n| n.min(TASK_MAX_TOOL_ROUNDS))
}

impl Machine for TaskMachine {
    type State = TaskState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> TaskState {
        TaskState::Top
    }

    fn initial(&self) -> TaskState {
        TaskState::Run
    }

    fn superstate(&self, _state: TaskState) -> TaskState {
        TaskState::Top
    }

    fn is_terminal(&self, state: TaskState) -> bool {
        state == TaskState::Done
    }

    fn all_states(&self) -> Vec<TaskState> {
        vec![TaskState::Run, TaskState::Done]
    }

    fn dispatch_state(
        &mut self,
        state: TaskState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<TaskState> {
        use TaskState::*;

        match state {
            Top | Done => Reaction::Ignored,

            Run => {
                // Tool results and tool approvals, in-state. A tool approval
                // goes to the parent's approver when the spawner connected
                // one; otherwise the executor refuses it and the next turn
                // tells the model why the call did not run.
                if let Some(reaction) = handle_tool_event(ctx, phase_continuation_turn, event) {
                    return reaction;
                }
                match event {
                    Event::Internal(InternalEvent::Entry) => {
                        let tools: Vec<String> =
                            prompts::WRITE_TOOLS.iter().map(|s| s.to_string()).collect();
                        init_loop(ctx, "task", &tools, "", task_max_rounds(ctx));
                        let req = prompts::task_request(&self.task);
                        Reaction::effects(vec![sven_hsm::effect::Effect::CallLlm { request: req }])
                    }
                    Event::LlmTurnComplete { .. } => match on_llm_turn_complete(ctx, event) {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            match super::parse_sdlc_decision(&text) {
                                Some(decision) => self.conclude(ctx, decision),
                                None => self.finish(
                                    ctx,
                                    "failed to parse decision".into(),
                                    false,
                                    Value::Null,
                                ),
                            }
                        }
                        GeneratingAction::CallTools { tool_effects, .. } => {
                            Reaction::effects(tool_effects)
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::effects(vec![nudge_effect])
                        }
                        GeneratingAction::MaxRoundsReached { .. } => {
                            self.finish(ctx, "max tool rounds reached".into(), false, Value::Null)
                        }
                    },
                    Event::UserMessage { text }
                        if ctx.fact(AWAITING_ANSWER_FACT).and_then(Value::as_bool)
                            == Some(true) =>
                    {
                        ctx.facts.remove(AWAITING_ANSWER_FACT);
                        let instruction = format!(
                            "The developer answered:\n\n\"{text}\"\n\nIncorporate this and \
                             finish the task."
                        );
                        Self::follow_up(ctx, &instruction)
                    }
                    Event::HumanApproved { approval_id } => {
                        ctx.approve(*approval_id);
                        Self::follow_up(ctx, "Approved. Finish the task.")
                    }
                    Event::HumanRejected { .. } => self.finish(
                        ctx,
                        "a person rejected the task's request for approval".into(),
                        false,
                        Value::Null,
                    ),
                    Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                        self.finish(ctx, format!("task failed: {error}"), false, Value::Null)
                    }
                    _ => Reaction::Super(Top),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::dispatch::Hsm;
    use sven_hsm::effect::Effect;

    fn decision_turn(decision: Value) -> Event {
        Event::LlmTurnComplete {
            thread: "task".into(),
            text: decision.to_string(),
            tool_calls: vec![],
        }
    }

    fn started(ctx: &mut Context) -> Hsm<TaskMachine> {
        let mut hsm = Hsm::new(TaskMachine::new("pick a database"));
        hsm.init(ctx);
        hsm
    }

    fn asks_for_input() -> Event {
        decision_turn(json!({"status": "need_user_input", "message": "Which database?"}))
    }

    fn result(ctx: &Context) -> Value {
        ctx.fact(TaskMachine::RESULT_FACT)
            .cloned()
            .unwrap_or(Value::Null)
    }

    #[test]
    fn a_child_connected_to_a_person_asks_and_carries_on_with_the_answer() {
        let mut ctx = Context::new();
        ctx.set_fact(TaskMachine::HUMAN_GATE_FACT, true);
        let mut hsm = started(&mut ctx);

        let asked = hsm.dispatch(&asks_for_input(), &mut ctx);
        assert!(
            matches!(&asked.effects[..], [Effect::AskUser { prompt }] if prompt.contains("Which database?")),
            "{:?}",
            asked.effects
        );
        assert!(!hsm.is_done(), "the task waits for the answer");

        let answered = hsm.dispatch(&Event::user_message("Postgres"), &mut ctx);
        let turn = answered
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::CallLlm { request } => Some(request.to_string()),
                _ => None,
            })
            .expect("a follow-up turn");
        assert!(turn.contains("Postgres"), "{turn}");

        hsm.dispatch(
            &decision_turn(json!({"status": "proceed", "message": "used Postgres"})),
            &mut ctx,
        );
        assert!(hsm.is_done());
        assert_eq!(result(&ctx)["ok"], json!(true));
    }

    #[test]
    fn a_child_nobody_can_answer_says_why_it_stopped() {
        let mut ctx = Context::new();
        let mut hsm = started(&mut ctx);
        hsm.dispatch(&asks_for_input(), &mut ctx);
        assert!(hsm.is_done());
        let out = result(&ctx);
        assert_eq!(out["ok"], json!(false));
        let summary = out["summary"].as_str().unwrap();
        assert!(summary.contains("no way to reach one"), "{summary}");
        assert!(summary.contains("Which database?"), "{summary}");
    }

    #[test]
    fn the_run_budget_lowers_the_task_round_cap_but_never_raises_it() {
        for (fact, expected) in [
            (Some(5), 5),
            (Some(500), TASK_MAX_TOOL_ROUNDS),
            (None, TASK_MAX_TOOL_ROUNDS),
        ] {
            let mut ctx = Context::new();
            if let Some(n) = fact {
                ctx.set_fact(MAX_TOOL_ROUNDS_FACT, n);
            }
            started(&mut ctx);
            assert_eq!(max_rounds(&ctx), u64::from(expected), "fact {fact:?}");
        }
    }
}
