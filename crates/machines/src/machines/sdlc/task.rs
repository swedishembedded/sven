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
//! `TaskMachine` uses the same kernel-mediated pattern as the other machines:
//! `Effect::CallLlm kind="turn"` → `LlmTurnComplete{text, tool_calls}` → if
//! `tool_calls` non-empty → `RunningTools` → continuation → final text is
//! parsed as decision JSON.

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    status::Reaction,
};

use super::decisions::decision_schema;
use super::decisions::{message_of, payload_of, status_of, DecisionStatus};
use super::prompts;
use crate::machines::loop_core::{
    build_turn_effect, init_loop, mark_calls_pending, max_rounds, on_llm_turn_complete,
    on_tool_result, GeneratingAction,
};

/// States of the one-shot task submachine.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TaskState {
    Top,
    Run,
    /// Tool calls dispatched; awaiting results.
    RunningTools,
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
            Top => Reaction::Ignored,

            Run => match event {
                Event::Internal(InternalEvent::Entry) => {
                    // Check continuation re-entry guard.
                    if ctx
                        .facts
                        .get("task_in_continuation")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        ctx.facts.remove("task_in_continuation");
                        return Reaction::handled();
                    }
                    // Fresh entry: start the task turn.
                    let tools_owned: Vec<String> =
                        prompts::WRITE_TOOLS.iter().map(|s| s.to_string()).collect();
                    init_loop(ctx, "task", &tools_owned, "", 40);
                    let req = prompts::task_request(&self.task);
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    let action = on_llm_turn_complete(ctx, event);
                    match action {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            // Parse the decision and store the result.
                            let decision = super::parse_sdlc_decision(&text).unwrap_or_else(|| {
                                json!({"status": "failed", "summary": "failed to parse decision"})
                            });
                            let ok = matches!(status_of(&decision), DecisionStatus::Proceed);
                            ctx.set_fact(
                                Self::RESULT_FACT,
                                json!({
                                    "task": self.task,
                                    "summary": message_of(&decision),
                                    "ok": ok,
                                    "payload": payload_of(&decision),
                                }),
                            );
                            Reaction::transition(Done, [], "task complete")
                        }
                        GeneratingAction::CallTools {
                            calls,
                            tool_effects,
                            ..
                        } => {
                            mark_calls_pending(ctx, &calls);
                            Reaction::transition(
                                RunningTools,
                                tool_effects,
                                "task: dispatching tool calls",
                            )
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::effects(vec![nudge_effect])
                        }
                        GeneratingAction::MaxRoundsReached { .. } => {
                            ctx.set_fact(
                                Self::RESULT_FACT,
                                json!({"task": self.task, "summary": "max tool rounds reached", "ok": false}),
                            );
                            Reaction::transition(Done, [], "task: max rounds reached")
                        }
                    }
                }
                Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                    ctx.set_fact(
                        Self::RESULT_FACT,
                        json!({
                            "task": self.task,
                            "summary": format!("task failed: {error}"),
                            "ok": false,
                        }),
                    );
                    Reaction::transition(Done, [], "task failed")
                }
                _ => Reaction::Super(Top),
            },

            RunningTools => match event {
                Event::ToolSucceeded { call_id, .. } | Event::ToolFailed { call_id, .. } => {
                    let all_done = on_tool_result(ctx, call_id);
                    if all_done {
                        // Build continuation turn with the decision schema.
                        let tools_owned: Vec<String> =
                            prompts::WRITE_TOOLS.iter().map(|s| s.to_string()).collect();
                        let next_turn = build_turn_effect(
                            "task",
                            &tools_owned,
                            "",
                            None,
                            None,
                            None,
                            max_rounds(ctx) as u32,
                            Some(decision_schema()),
                            Some("decision"),
                        );
                        ctx.set_fact("task_in_continuation", json!(true));
                        Reaction::transition(
                            Run,
                            vec![next_turn],
                            "task: all tools done; continuing",
                        )
                    } else {
                        Reaction::handled()
                    }
                }
                Event::ToolApprovalRequired {
                    call_id,
                    capability,
                    description,
                } => {
                    // Auto-deny in task context (no human approver).
                    let _ = on_tool_result(ctx, call_id);
                    // Emit a RequestHumanApproval for the kernel to handle.
                    // In headless mode, child kernels resolve approval as ToolFailed (Phase D2).
                    let _ = (capability, description);
                    Reaction::handled()
                }
                _ => Reaction::Super(Top),
            },

            Done => Reaction::Ignored,
        }
    }
}
