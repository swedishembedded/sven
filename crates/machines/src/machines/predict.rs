// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! One schema-constrained, tool-free turn per message.
//!
//! This is the execution strategy behind a typed model-driven method whose job
//! is to produce a value rather than to investigate: classification,
//! extraction, assessment, summarisation. The model is given the return type's
//! JSON schema and **no tools at all**.
//!
//! ```text
//! Top
//!  └── Session                    (UserCancelled → Idle)
//!       ├── Idle                  ← waiting for a task
//!       └── Generating            ← one constrained turn in flight
//! ```
//!
//! # Why the validation loop is not in here
//!
//! A transition cannot deserialise a candidate into the caller's return type -
//! the machine has no idea what that type is, and finding out would mean
//! reaching for the caller's code from inside a pure function. So the machine
//! does one thing well: it runs a constrained turn and records the raw
//! candidate in [`RESULT_FACT`].
//!
//! Validating that candidate, and deciding whether a failure is worth another
//! attempt, belongs to whoever declared the return type. A repair arrives back
//! here as an ordinary `UserMessage` carrying the diagnostic, which is why
//! `Generating` returns to `Idle` on every outcome rather than holding a retry
//! budget of its own. That keeps the correction loop bounded by explicit code
//! in the caller - see `sven_sdk::Method` - and keeps this machine honest about
//! what it can actually decide.
//!
//! Swedish Embedded AB implements deterministic agent execution strategies for
//! its clients. If your team needs expertise in constrained model execution
//! then you can procure our services by sending an email to
//! info@swedishembedded.com.

use serde_json::{json, Value};
use sven_hsm::{event::InternalEvent, Context, Effect, Event, Machine, MachineId, Reaction};

use super::loop_core::build_turn_effect;

/// The conversation thread a prediction runs in.
pub const PREDICT_THREAD: &str = "predict";

/// Context fact holding the JSON schema the response must conform to.
///
/// Seeded by the caller before the first message; see
/// `sven_bootstrap::RuntimeBuilder::with_context_facts`.
pub const SCHEMA_FACT: &str = "predict.schema";

/// Context fact holding the schema's name (the method name, for providers
/// that require one).
pub const SCHEMA_NAME_FACT: &str = "predict.schema_name";

/// Context fact holding the raw text of the most recent candidate.
pub const RESULT_FACT: &str = "predict.candidate";

/// Context fact holding the most recent provider failure.
pub const ERROR_FACT: &str = "predict.error";

/// A prediction turn allows no tool calls, so it needs no tool rounds.
const NO_TOOL_ROUNDS: u32 = 0;

/// States of the prediction machine.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PredictState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of all session states; handles `UserCancelled`.
    Session,
    /// Waiting for a task.
    Idle,
    /// One constrained turn in flight.
    Generating,
}

/// Runs one schema-constrained, tool-free turn per message.
pub struct PredictMachine {
    id: MachineId,
}

impl PredictMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// Builds the constrained turn for `text`, carrying the seeded schema.
    fn turn_effect(ctx: &Context, text: &str) -> Effect {
        let schema = ctx.facts.get(SCHEMA_FACT).cloned();
        let schema_name = ctx
            .facts
            .get(SCHEMA_NAME_FACT)
            .and_then(Value::as_str)
            .unwrap_or("result")
            .to_string();
        build_turn_effect(
            PREDICT_THREAD,
            // No named tools, and - just as importantly - no all-tools mode to
            // fall back on. An empty mode string is what stops the executor
            // offering the whole registry.
            &[],
            "",
            None,
            Some(text),
            None,
            NO_TOOL_ROUNDS,
            schema,
            Some(&schema_name),
        )
    }
}

impl Default for PredictMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine for PredictMachine {
    type State = PredictState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> PredictState {
        PredictState::Top
    }

    fn initial(&self) -> PredictState {
        PredictState::Session
    }

    fn superstate(&self, state: PredictState) -> PredictState {
        match state {
            PredictState::Top => PredictState::Top,
            PredictState::Session => PredictState::Top,
            PredictState::Idle | PredictState::Generating => PredictState::Session,
        }
    }

    fn all_states(&self) -> Vec<PredictState> {
        // `Top` is the implicit root, never an active leaf, so not resumable.
        vec![
            PredictState::Session,
            PredictState::Idle,
            PredictState::Generating,
        ]
    }

    fn dispatch_state(
        &mut self,
        state: PredictState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<PredictState> {
        use PredictState::{Generating, Idle, Session, Top};

        match state {
            Top => Reaction::Ignored,

            Session => match event {
                Event::Internal(InternalEvent::Init) => Reaction::goto(Idle),
                Event::UserCancelled => {
                    Reaction::transition(Idle, vec![], "user cancelled; returning to Idle")
                }
                _ => Reaction::Super(Top),
            },

            Idle => match event {
                Event::UserMessage { text } => Reaction::transition(
                    Generating,
                    vec![Self::turn_effect(ctx, text)],
                    "task received; starting a constrained turn",
                ),
                _ => Reaction::Super(Session),
            },

            // Every outcome returns to Idle. A prediction turn has nothing to
            // continue into: there are no tools to run, and a repair attempt
            // arrives as the next message.
            Generating => match event {
                Event::LlmTurnComplete { text, .. } => {
                    ctx.set_fact(RESULT_FACT, json!(text));
                    Reaction::transition(Idle, vec![], "turn complete; candidate recorded")
                }
                Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                    ctx.set_fact(ERROR_FACT, json!(error));
                    Reaction::transition(Idle, vec![], "turn failed")
                }
                _ => Reaction::Super(Session),
            },
        }
    }
}
