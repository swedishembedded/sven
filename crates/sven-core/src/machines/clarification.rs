//! Reusable clarification submachine.
//!
//! [`ClarificationMachine`] collects one round of clarifying information from
//! the user. It can be instantiated as a submachine by any state that discovers
//! incomplete information. When it reaches a terminal [`ClarState::DoneEnough`]
//! or [`ClarState::DoneNotEnough`] state, the parent machine receives an
//! [`InternalEvent::SubmachineCompleted`] and can resume from the stored
//! continuation state.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! ├── GeneratingQuestion  ← LLM drafts a clarifying question
//! ├── AwaitingAnswer      ← question shown to user; waiting for reply
//! ├── InterpretingAnswer  ← LLM interprets the user's reply
//! ├── Deciding            ← LLM decides if we have enough info now
//! ├── DoneEnough          ← terminal: clarification succeeded
//! └── DoneNotEnough       ← terminal: could not get enough info
//! ```

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    status::Reaction,
};

/// States of the clarification machine (flat enum; all are direct children of
/// `Top` since this is a shallow hierarchy).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClarState {
    /// Root fixpoint.
    Top,
    /// LLM is generating a clarifying question.
    GeneratingQuestion,
    /// Question has been presented; waiting for the user to reply.
    AwaitingAnswer,
    /// LLM is interpreting the user's answer.
    InterpretingAnswer,
    /// LLM deciding whether the information is now sufficient.
    Deciding,
    /// Terminal: we now have enough information.
    DoneEnough,
    /// Terminal: the user could not provide sufficient information.
    DoneNotEnough,
}

/// The clarification submachine.
pub struct ClarificationMachine {
    id: MachineId,
}

impl Default for ClarificationMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ClarificationMachine {
    /// Creates a new instance.
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }
}

impl Machine for ClarificationMachine {
    type State = ClarState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> ClarState {
        ClarState::Top
    }

    fn initial(&self) -> ClarState {
        ClarState::GeneratingQuestion
    }

    fn superstate(&self, state: ClarState) -> ClarState {
        // Shallow: every state's parent is Top.
        match state {
            ClarState::Top => ClarState::Top,
            _ => ClarState::Top,
        }
    }

    fn dispatch_state(
        &mut self,
        state: ClarState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<ClarState> {
        use ClarState::*;

        match state {
            Top => Reaction::Ignored,

            GeneratingQuestion => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::effects(vec![Effect::CallLlm {
                    request: json!({ "kind": "GenerateClarifyingQuestion" }),
                }]),
                Event::LlmProposedResponse { text } => Reaction::transition(
                    AwaitingAnswer,
                    vec![Effect::AskUser {
                        prompt: text.clone(),
                    }],
                    "LLM generated clarifying question; presenting to user",
                ),
                Event::LlmFailed { error } => {
                    ctx.set_fact("clarification_error", json!(error));
                    Reaction::goto(DoneNotEnough)
                }
                _ => Reaction::Ignored,
            },

            AwaitingAnswer => match event {
                Event::UserMessage { text } => {
                    ctx.set_fact("user_answer", json!(text));
                    Reaction::transition(
                        InterpretingAnswer,
                        vec![Effect::CallLlm {
                            request: json!({
                                "kind": "InterpretUserAnswer",
                                "answer": text,
                            }),
                        }],
                        "user provided answer; interpreting",
                    )
                }
                Event::UserCancelled => {
                    ctx.set_fact("clarification_cancelled", json!(true));
                    Reaction::goto(DoneNotEnough)
                }
                _ => Reaction::Ignored,
            },

            InterpretingAnswer => match event {
                Event::LlmProposedAssessment { assessment } => {
                    ctx.set_fact("interpreted_answer", assessment.clone());
                    Reaction::goto(Deciding)
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact("clarification_error", json!(error));
                    Reaction::goto(DoneNotEnough)
                }
                _ => Reaction::Ignored,
            },

            Deciding => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::effects(vec![Effect::CallLlm {
                    request: json!({ "kind": "AssessCompleteness" }),
                }]),
                Event::LlmProposedAssessment { assessment } => {
                    let enough = assessment
                        .get("completeness")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        == "enough";
                    if enough {
                        Reaction::goto(DoneEnough)
                    } else {
                        // Could loop back to GeneratingQuestion; for simplicity
                        // we give up after one round (caller can retry).
                        Reaction::goto(DoneNotEnough)
                    }
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact("clarification_error", json!(error));
                    Reaction::goto(DoneNotEnough)
                }
                _ => Reaction::Ignored,
            },

            // Terminal states absorb all events.
            DoneEnough | DoneNotEnough => Reaction::Ignored,
        }
    }

    fn is_terminal(&self, state: ClarState) -> bool {
        matches!(state, ClarState::DoneEnough | ClarState::DoneNotEnough)
    }

    fn all_states(&self) -> Vec<ClarState> {
        vec![
            ClarState::Top,
            ClarState::GeneratingQuestion,
            ClarState::AwaitingAnswer,
            ClarState::InterpretingAnswer,
            ClarState::Deciding,
            ClarState::DoneEnough,
            ClarState::DoneNotEnough,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{dispatch::Hsm, effect::EffectKind};

    fn make_hsm() -> (Hsm<ClarificationMachine>, Context) {
        let mut hsm = Hsm::new(ClarificationMachine::new());
        let mut ctx = Context::new();
        let entry_effects = hsm.init(&mut ctx);
        // Entry into GeneratingQuestion fires a CallLlm
        assert!(entry_effects
            .iter()
            .any(|e| e.kind() == EffectKind::CallLlm));
        (hsm, ctx)
    }

    #[test]
    fn initial_state_is_generating_question() {
        let (hsm, _) = make_hsm();
        assert_eq!(hsm.state(), ClarState::GeneratingQuestion);
    }

    #[test]
    fn happy_path_reaches_done_enough() {
        let (mut hsm, mut ctx) = make_hsm();

        // LLM generates a question.
        let out = hsm.dispatch(
            &Event::LlmProposedResponse {
                text: "Which environment are you targeting?".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ClarState::AwaitingAnswer);
        assert!(out.effects.iter().any(|e| e.kind() == EffectKind::AskUser));

        // User answers.
        hsm.dispatch(&Event::user_message("production"), &mut ctx);
        assert_eq!(hsm.state(), ClarState::InterpretingAnswer);

        // LLM produces an assessment of the answer.
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "summary": "production env" }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ClarState::Deciding);

        // LLM decides we have enough info.
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "completeness": "enough" }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ClarState::DoneEnough);
        assert!(hsm.is_done());
    }

    #[test]
    fn user_cancel_reaches_done_not_enough() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(
            &Event::LlmProposedResponse {
                text: "Please clarify.".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ClarState::AwaitingAnswer);

        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ClarState::DoneNotEnough);
        assert!(hsm.is_done());
    }

    #[test]
    fn llm_failure_reaches_done_not_enough() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(
            &Event::LlmFailed {
                error: "upstream timeout".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ClarState::DoneNotEnough);
        assert!(hsm.is_done());
    }
}
