//! The default chat machine.
//!
//! [`ConversationMachine`] is the normal turn-by-turn conversation loop.
//! When the LLM classifies the user's request as a large SDLC task it emits
//! [`Effect::InstantiateSubmachine`] with a descriptor that tells the runtime
//! to spin up a [`super::software_development::SoftwareDevelopmentMachine`].
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Active
//!     ├── Idle           ← waiting for user
//!     ├── Interpreting   ← LLM extracting intent
//!     ├── Responding     ← LLM generating response
//!     ├── AwaitingTool   ← tool call in flight
//!     └── AwaitingUser   ← clarification needed
//! ```

use serde_json::{json, Value};
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::{MachineId, ToolCallId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

/// States of the conversation machine (flat enum; hierarchy via `superstate`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConvState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of all conversational states.
    Active,
    /// Waiting for the user's next message.
    Idle,
    /// LLM is extracting intent from the user message.
    Interpreting,
    /// LLM is generating a response / continuing the turn.
    Responding,
    /// A tool call is in flight; waiting for `ToolSucceeded`/`ToolFailed`.
    AwaitingTool,
    /// Machine asked the user a clarifying question; waiting for their answer.
    AwaitingUser,
}

/// The default conversation machine.
///
/// Stateless beyond its [`MachineId`]; all domain facts live in the
/// [`Context`] passed to every dispatch.
pub struct ConversationMachine {
    id: MachineId,
}

impl Default for ConversationMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ConversationMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// The permission policy for conversation: read and network access are
    /// always allowed; write / git operations require prior human approval.
    pub fn permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile, ToolCapability::NetworkAccess])
            .require_approval([ToolCapability::WriteFile, ToolCapability::GitOperation])
            .build()
    }
}

impl Machine for ConversationMachine {
    type State = ConvState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> ConvState {
        ConvState::Top
    }

    /// The machine starts by entering `Active`, whose `Init` handler then
    /// drills into `Idle`.
    fn initial(&self) -> ConvState {
        ConvState::Active
    }

    fn superstate(&self, state: ConvState) -> ConvState {
        match state {
            ConvState::Top => ConvState::Top,
            ConvState::Active => ConvState::Top,
            ConvState::Idle
            | ConvState::Interpreting
            | ConvState::Responding
            | ConvState::AwaitingTool
            | ConvState::AwaitingUser => ConvState::Active,
        }
    }

    fn dispatch_state(
        &mut self,
        state: ConvState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<ConvState> {
        use ConvState::*;

        match state {
            // -----------------------------------------------------------------
            // Root: catches nothing specific; Active handles UserCancelled.
            // -----------------------------------------------------------------
            Top => Reaction::Ignored,

            // -----------------------------------------------------------------
            // Active (composite)
            // -----------------------------------------------------------------
            Active => match event {
                Event::Internal(InternalEvent::Init) => Reaction::goto(Idle),
                Event::UserCancelled => Reaction::transition(
                    Idle,
                    vec![],
                    "user cancelled in-flight work, returning to Idle",
                ),
                _ => Reaction::Super(Top),
            },

            // -----------------------------------------------------------------
            // Idle
            // -----------------------------------------------------------------
            Idle => match event {
                Event::UserMessage { text } => {
                    // The request shape must match `sven_llm::LlmRequest`'s serde
                    // contract: `#[serde(tag = "kind", rename_all = "snake_case")]`.
                    // The previous PascalCase tag / `user_message` field never
                    // deserialised, so every chat turn failed at the adapter.
                    let req = json!({
                        "kind": "extract_intent",
                        "text": text,
                        "allowed_intents": ["chat", "question", "code", "large_task"],
                    });
                    Reaction::transition(
                        Interpreting,
                        vec![Effect::CallLlm { request: req }],
                        "user sent message; extracting intent",
                    )
                }
                _ => Reaction::Super(Active),
            },

            // -----------------------------------------------------------------
            // Interpreting
            // -----------------------------------------------------------------
            Interpreting => match event {
                Event::LlmProposedAssessment { assessment } => {
                    let is_large = assessment
                        .get("is_large_task")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);

                    if is_large {
                        let machine_id = MachineId::new();
                        let descriptor = json!({ "kind": "SoftwareDevelopmentMachine" });
                        ctx.set_fact("sdlc_machine_id", json!(machine_id.as_uuid().to_string()));
                        Reaction::transition(
                            Idle,
                            vec![Effect::InstantiateSubmachine {
                                machine: machine_id,
                                descriptor,
                            }],
                            "large SDLC task detected; delegating to SoftwareDevelopmentMachine",
                        )
                    } else {
                        let req = json!({
                            "kind": "generate_response",
                            "intent": assessment,
                        });
                        Reaction::transition(
                            Responding,
                            vec![Effect::CallLlm { request: req }],
                            "simple chat intent; generating response",
                        )
                    }
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact("last_error", json!(error));
                    Reaction::goto(Idle)
                }
                _ => Reaction::Super(Active),
            },

            // -----------------------------------------------------------------
            // Responding
            // -----------------------------------------------------------------
            Responding => match event {
                Event::LlmProposedResponse { text } => Reaction::transition(
                    Idle,
                    vec![Effect::AskUser {
                        prompt: text.clone(),
                    }],
                    "LLM finished response; showing to user",
                ),
                Event::LlmProposedToolCall { name, args } => {
                    let call_id = ToolCallId::new();
                    ctx.set_fact("pending_tool_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::transition(
                        AwaitingTool,
                        vec![Effect::CallTool {
                            call_id,
                            name: name.clone(),
                            capability: ToolCapability::ReadFile,
                            args: args.clone(),
                        }],
                        "LLM proposed tool call",
                    )
                }
                Event::LlmProposedAssessment { assessment }
                    if assessment.get("kind").and_then(Value::as_str).unwrap_or("")
                        == "needs_clarification" =>
                {
                    let question = assessment
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or("Could you clarify?")
                        .to_string();
                    Reaction::transition(
                        AwaitingUser,
                        vec![Effect::AskUser { prompt: question }],
                        "LLM needs clarification from user",
                    )
                }
                Event::LlmFailed { error } => {
                    ctx.set_fact("last_error", json!(error));
                    Reaction::goto(Idle)
                }
                _ => Reaction::Super(Active),
            },

            // -----------------------------------------------------------------
            // AwaitingTool
            // -----------------------------------------------------------------
            AwaitingTool => match event {
                Event::ToolSucceeded { .. } | Event::ToolFailed { .. } => {
                    Reaction::goto(Responding)
                }
                _ => Reaction::Super(Active),
            },

            // -----------------------------------------------------------------
            // AwaitingUser
            // -----------------------------------------------------------------
            AwaitingUser => match event {
                Event::UserMessage { .. } => Reaction::goto(Responding),
                _ => Reaction::Super(Active),
            },
        }
    }

    fn all_states(&self) -> Vec<ConvState> {
        vec![
            ConvState::Top,
            ConvState::Active,
            ConvState::Idle,
            ConvState::Interpreting,
            ConvState::Responding,
            ConvState::AwaitingTool,
            ConvState::AwaitingUser,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{dispatch::Hsm, effect::EffectKind, ids::ToolCallId};

    fn make_hsm() -> (Hsm<ConversationMachine>, Context) {
        let mut hsm = Hsm::new(ConversationMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        (hsm, ctx)
    }

    // ------------------------------------------------------------------
    // Initial state
    // ------------------------------------------------------------------

    #[test]
    fn initial_state_is_idle() {
        let (hsm, _) = make_hsm();
        assert_eq!(hsm.state(), ConvState::Idle);
    }

    // ------------------------------------------------------------------
    // Idle → Interpreting
    // ------------------------------------------------------------------

    #[test]
    fn idle_user_message_emits_extract_intent_and_enters_interpreting() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(
            &Event::user_message("refactor the billing module"),
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Interpreting);
        assert!(out.transitioned);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
        if let Effect::CallLlm { request } = &out.effects[0] {
            assert_eq!(request["kind"], "extract_intent");
        } else {
            panic!("expected CallLlm");
        }
    }

    // ------------------------------------------------------------------
    // Interpreting → Responding (simple chat)
    // ------------------------------------------------------------------

    #[test]
    fn interpreting_simple_intent_enters_responding_with_generate_response() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hello"), &mut ctx);

        let assessment = json!({ "is_large_task": false, "intent": "greeting" });
        let out = hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: assessment.clone(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Responding);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
        if let Effect::CallLlm { request } = &out.effects[0] {
            assert_eq!(request["kind"], "generate_response");
        } else {
            panic!("expected CallLlm");
        }
    }

    // ------------------------------------------------------------------
    // Interpreting → Idle + InstantiateSubmachine (large SDLC task)
    // ------------------------------------------------------------------

    #[test]
    fn interpreting_large_task_emits_instantiate_submachine_and_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(
            &Event::user_message("migrate the entire database schema"),
            &mut ctx,
        );

        let assessment = json!({ "is_large_task": true });
        let out = hsm.dispatch(&Event::LlmProposedAssessment { assessment }, &mut ctx);
        assert_eq!(hsm.state(), ConvState::Idle);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::InstantiateSubmachine);
        if let Effect::InstantiateSubmachine { descriptor, .. } = &out.effects[0] {
            assert_eq!(descriptor["kind"], "SoftwareDevelopmentMachine");
        } else {
            panic!("expected InstantiateSubmachine");
        }
    }

    // ------------------------------------------------------------------
    // Responding → Idle (text complete)
    // ------------------------------------------------------------------

    #[test]
    fn responding_llm_response_emits_ask_user_and_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_responding(&mut hsm, &mut ctx);

        let out = hsm.dispatch(
            &Event::LlmProposedResponse {
                text: "Here is your answer.".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Idle);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::AskUser);
        if let Effect::AskUser { prompt } = &out.effects[0] {
            assert_eq!(prompt, "Here is your answer.");
        } else {
            panic!("expected AskUser");
        }
    }

    // ------------------------------------------------------------------
    // Responding → AwaitingTool
    // ------------------------------------------------------------------

    #[test]
    fn responding_tool_call_emits_call_tool_and_enters_awaiting_tool() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_responding(&mut hsm, &mut ctx);

        let out = hsm.dispatch(
            &Event::LlmProposedToolCall {
                name: "read_file".into(),
                args: json!({ "path": "Cargo.toml" }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::AwaitingTool);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallTool);
    }

    // ------------------------------------------------------------------
    // AwaitingTool → Responding
    // ------------------------------------------------------------------

    #[test]
    fn awaiting_tool_succeeded_returns_to_responding() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_awaiting_tool(&mut hsm, &mut ctx);

        let out = hsm.dispatch(
            &Event::ToolSucceeded {
                call_id: ToolCallId::new(),
                observation: json!({ "content": "..." }),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Responding);
        assert!(out.transitioned);
    }

    #[test]
    fn awaiting_tool_failed_returns_to_responding() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_awaiting_tool(&mut hsm, &mut ctx);

        let out = hsm.dispatch(
            &Event::ToolFailed {
                call_id: ToolCallId::new(),
                error: "not found".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Responding);
        assert!(out.transitioned);
    }

    // ------------------------------------------------------------------
    // UserCancelled from any sub-state → Idle
    // ------------------------------------------------------------------

    #[test]
    fn user_cancelled_from_interpreting_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("long task"), &mut ctx);
        assert_eq!(hsm.state(), ConvState::Interpreting);

        let out = hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ConvState::Idle);
        assert!(out.transitioned);
    }

    #[test]
    fn user_cancelled_from_awaiting_tool_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_awaiting_tool(&mut hsm, &mut ctx);

        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ConvState::Idle);
    }

    #[test]
    fn user_cancelled_from_responding_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_responding(&mut hsm, &mut ctx);

        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ConvState::Idle);
    }

    // ------------------------------------------------------------------
    // AwaitingUser → Responding
    // ------------------------------------------------------------------

    #[test]
    fn awaiting_user_message_transitions_to_responding() {
        let (mut hsm, mut ctx) = make_hsm();
        drive_to_awaiting_user(&mut hsm, &mut ctx);
        assert_eq!(hsm.state(), ConvState::AwaitingUser);

        hsm.dispatch(&Event::user_message("I meant the auth module"), &mut ctx);
        assert_eq!(hsm.state(), ConvState::Responding);
    }

    // ------------------------------------------------------------------
    // LlmFailed
    // ------------------------------------------------------------------

    #[test]
    fn llm_failed_in_interpreting_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("?"), &mut ctx);

        hsm.dispatch(
            &Event::LlmFailed {
                error: "timeout".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ConvState::Idle);
    }

    // ------------------------------------------------------------------
    // Helpers
    // ------------------------------------------------------------------

    fn drive_to_responding(hsm: &mut Hsm<ConversationMachine>, ctx: &mut Context) {
        hsm.dispatch(&Event::user_message("what is 2+2"), ctx);
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({ "is_large_task": false }),
            },
            ctx,
        );
        assert_eq!(hsm.state(), ConvState::Responding);
    }

    fn drive_to_awaiting_tool(hsm: &mut Hsm<ConversationMachine>, ctx: &mut Context) {
        drive_to_responding(hsm, ctx);
        hsm.dispatch(
            &Event::LlmProposedToolCall {
                name: "read_file".into(),
                args: json!({}),
            },
            ctx,
        );
        assert_eq!(hsm.state(), ConvState::AwaitingTool);
    }

    fn drive_to_awaiting_user(hsm: &mut Hsm<ConversationMachine>, ctx: &mut Context) {
        drive_to_responding(hsm, ctx);
        hsm.dispatch(
            &Event::LlmProposedAssessment {
                assessment: json!({
                    "kind": "needs_clarification",
                    "question": "Which module did you mean?",
                }),
            },
            ctx,
        );
    }
}
