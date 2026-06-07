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

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    status::Reaction,
};

use super::decisions::{message_of, payload_of, status_of, DecisionStatus};

/// States of the one-shot task submachine.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TaskState {
    Top,
    Run,
    Done,
}

/// A child submachine that deliberates a single task to completion.
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
        match state {
            TaskState::Top => Reaction::Ignored,
            TaskState::Run => match event {
                Event::Internal(InternalEvent::Entry) => {
                    let req = super::prompts::task_request(&self.task);
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => {
                    let ok = matches!(status_of(decision), DecisionStatus::Proceed);
                    ctx.set_fact(
                        Self::RESULT_FACT,
                        json!({
                            "task": self.task,
                            "summary": message_of(decision),
                            "ok": ok,
                            "payload": payload_of(decision),
                        }),
                    );
                    Reaction::transition(TaskState::Done, [], "task complete")
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact(
                        Self::RESULT_FACT,
                        json!({
                            "task": self.task,
                            "summary": format!("task failed: {error}"),
                            "ok": false,
                        }),
                    );
                    Reaction::transition(TaskState::Done, [], "task failed")
                }
                _ => Reaction::Super(TaskState::Top),
            },
            TaskState::Done => Reaction::Ignored,
        }
    }
}
