// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The default coding-agent machine.
//!
//! [`ReactiveAgentMachine`] models the full turn lifecycle of a streaming,
//! native-tool-calling coding agent.  It uses the shared [`super::loop_core`]
//! helpers and runs the tool loop **in-state** — there are no separate
//! `RunningTools` or `AwaitingApproval` states.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Session
//!     ├── Idle        ← waiting for the user's next message
//!     └── Generating  ← LLM turn in flight; tools executed in-state
//! ```
//!
//! # In-state tool loop
//!
//! On `UserMessage`, the machine initialises `LoopState` and emits a
//! `CallLlm kind="turn"`.  `TurnExecutor` streams the model and posts
//! `LlmTurnComplete`.  If the model proposed tool calls the machine emits
//! `CallTool` effects **and stays in `Generating`** (`Reaction::Handled`).
//! `ToolSucceeded` / `ToolFailed` / `ToolApprovalRequired` / `HumanApproved`
//! (tool) / `HumanRejected` (tool) are all handled by the shared
//! [`handle_tool_event`](super::loop_core::handle_tool_event) helper which
//! drains the pending set, requests human approval, and emits continuation
//! turns — all while staying in `Generating`.
//!
//! A tool-free `LlmTurnComplete` transitions the machine to `Idle`.
//!
//! # The bounded clarification post-check
//!
//! `Generating` has no separate "needs clarification" state — adding one would
//! contradict the shape above. Instead the `FinalAnswer` branch runs a bounded
//! post-check: when the model's own final answer leaves the request unresolved,
//! the machine emits one `ask_question` `CallTool` and **stays** in
//! `Generating`, so the loop resumes with the user's pick instead of dead-ending
//! in `Idle`. It is bounded by the same [`DEFAULT_MAX_TOOL_ROUNDS`] budget as
//! the tool loop.
//!
//! The check is deliberately evidence-based and never inventive: it fires only
//! when the model **itself** asked the user a question and **itself** listed the
//! alternatives (see [`clarification_question`]). The machine supplies neither.
//! Anything else — a plain answer, a rhetorical question, a bare "I'm not sure"
//! with no alternatives — is left byte-identical to the previous behaviour,
//! because this path is on every turn of the default mode.

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::{MachineId, ToolCallId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

use super::loop_core::{
    build_turn_effect, handle_tool_event, init_loop, on_llm_turn_complete, GeneratingAction,
    LoopState,
};

/// The conversation thread name used by the reactive agent.
pub const CHAT_THREAD: &str = "chat";

/// Default maximum tool-call rounds before a forced wrap-up turn.
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 16;

/// Default mode for all-tools resolution.
const AGENT_MODE: &str = "agent";

/// The tool the clarification post-check calls.
///
/// Named by string, not by type: `sven-machines` is a machines-tier crate and
/// has no legal dependency edge to the domain-tier `sven-tools-agent` that owns
/// the implementation.
const ASK_QUESTION_TOOL: &str = "ask_question";

/// The capability `ask_question` is classified under.
///
/// Mirrors `AskQuestionTool::kernel_capability()`; the same reason as
/// [`ASK_QUESTION_TOOL`] keeps it a literal here rather than a lookup. The
/// kernel still enforces it — this only declares which bucket the call falls in.
const ASK_QUESTION_CAPABILITY: ToolCapability = ToolCapability::ReadFile;

/// Bounds on how many alternatives make a sensible multiple-choice question.
/// Fewer is not a choice; more is a list the user should read as prose.
const MIN_ALTERNATIVES: usize = 2;
/// Upper bound, see [`MIN_ALTERNATIVES`].
const MAX_ALTERNATIVES: usize = 6;

/// Openers that mark a question as addressed to the *user* rather than
/// rhetorical. Matched case-insensitively against the question sentence.
const USER_DIRECTED_OPENERS: &[&str] = &[
    "which",
    "should i",
    "shall i",
    "do you",
    "would you",
    "could you",
    "can you",
];

/// States of the reactive agent machine.
///
/// `Generating` now owns the full tool loop — no separate `RunningTools` or
/// `AwaitingApproval` states.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReactiveState {
    /// Root (fixpoint: `superstate(Top) == Top`). Never the active leaf.
    Top,
    /// Composite parent of all session states; handles `UserCancelled`.
    Session,
    /// Waiting for the user's next message.
    Idle,
    /// LLM turn in flight; tool calls executed in-state.
    Generating,
}

/// The default streaming coding-agent machine.
pub struct ReactiveAgentMachine {
    id: MachineId,
}

impl Default for ReactiveAgentMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ReactiveAgentMachine {
    /// Creates a new instance with a fresh [`MachineId`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// The permission policy for the general coding agent (global allow for
    /// common capabilities; approval required for destructive rollback).
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::NetworkAccess,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
                ToolCapability::AssimilateKnowledge,
            ])
            .require_approval([ToolCapability::Rollback])
            .build()
    }

    /// The permission policy for read-only planning modes (`Plan` / `Research`).
    ///
    /// Identical to [`permission_policy`](Self::permission_policy) except that
    /// [`ToolCapability::WriteFile`] is **not** granted, so any file-mutating
    /// tool call the model proposes is classified `Forbidden` by the kernel and
    /// turned into a `ToolFailed` without ever reaching the executor. This is
    /// what makes plan mode structurally read-only: the guarantee holds even if
    /// the model ignores the (write-free) tool schemas and emits a write anyway.
    ///
    /// [`ToolCapability::AssimilateKnowledge`] *is* granted here: research and
    /// planning are precisely when the agent learns, and assimilation writes
    /// nothing into the user's workspace. What reaches durable storage is
    /// gated by the fact's provenance, not by the mode.
    #[must_use]
    pub fn plan_permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::NetworkAccess,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
                ToolCapability::AssimilateKnowledge,
            ])
            .require_approval([ToolCapability::Rollback])
            .build()
    }

    /// Build the first `CallLlm { kind:"turn" }` effect for a user message.
    fn first_turn_effect(text: &str) -> Effect {
        build_turn_effect(
            CHAT_THREAD,
            &[],
            AGENT_MODE,
            None,
            Some(text),
            None,
            DEFAULT_MAX_TOOL_ROUNDS,
            None,
            None,
        )
    }

    /// Build a continuation turn from the current `LoopState`.
    fn continuation_turn(ls: &LoopState) -> Effect {
        ls.continuation_turn()
    }

    /// The bounded clarification post-check on the `FinalAnswer` branch.
    ///
    /// Returns the single `ask_question` `CallTool` to emit — registered as
    /// pending so the shared tool loop resumes the turn when the user answers —
    /// or `None` to finish the turn exactly as before.
    fn clarification_effect(ctx: &mut Context, text: &str) -> Option<Effect> {
        let mut ls = LoopState::load(ctx);
        // `on_llm_turn_complete` has already counted this turn. Spending a round
        // past the budget is what the budget exists to prevent, so the check is
        // simply skipped there and the answer stands.
        if ls.round > ls.max_rounds {
            return None;
        }

        let (prompt, options) = clarification_question(text)?;
        let call_id = derive_call_id(&ls.thread, ls.round);
        ls.pending.insert(call_id);
        ls.store(ctx);

        Some(Effect::CallTool {
            call_id,
            name: ASK_QUESTION_TOOL.to_string(),
            capability: ASK_QUESTION_CAPABILITY,
            args: json!({
                "questions": [{
                    "prompt": prompt,
                    "options": options,
                    "allow_multiple": false,
                }]
            }),
        })
    }
}

/// Derive a deterministic [`ToolCallId`] for the clarification call.
///
/// Transitions must stay pure for event-sourced replay: minting a random id
/// here would make a replayed run register a different pending id than the one
/// in the recorded `ToolSucceeded`, so the pending set would never drain and
/// the loop would stall. `loop_core` derives its `ApprovalId` from the call it
/// gates for exactly this reason.
///
/// FNV-1a rather than a UUIDv5 namespace hash: it is a handful of lines, needs
/// no extra `uuid` feature or transitive dependency, and the only property
/// required here is that the same `(thread, round)` always yields the same id.
fn derive_call_id(thread: &str, round: u32) -> ToolCallId {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn fnv1a(seed: u64, bytes: &[u8]) -> u64 {
        bytes
            .iter()
            .fold(seed, |h, b| (h ^ u64::from(*b)).wrapping_mul(FNV_PRIME))
    }

    let key = format!("clarify:{thread}:{round}");
    let hi = fnv1a(FNV_OFFSET, key.as_bytes());
    let lo = fnv1a(FNV_OFFSET ^ FNV_PRIME, key.as_bytes());
    ToolCallId::from_uuid(uuid::Uuid::from_u128(
        (u128::from(hi) << 64) | u128::from(lo),
    ))
}

/// Extract a clarification question from a final answer, or `None` when the
/// answer is resolved.
///
/// Both halves must come from the model: a question it addressed to the user,
/// and between [`MIN_ALTERNATIVES`] and [`MAX_ALTERNATIVES`] alternatives it
/// enumerated itself. Requiring both is what keeps a resolved answer — including
/// one that happens to contain a rhetorical question, or a bullet list of work
/// done — untouched.
fn clarification_question(text: &str) -> Option<(String, Vec<String>)> {
    let prompt = user_directed_question(text)?;
    let options = enumerated_alternatives(text);
    if !(MIN_ALTERNATIVES..=MAX_ALTERNATIVES).contains(&options.len()) {
        return None;
    }
    Some((prompt, options))
}

/// The first question sentence that is addressed to the user, if any.
fn user_directed_question(text: &str) -> Option<String> {
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '?' => {
                let sentence = text[start..i + ch.len_utf8()].trim();
                if is_user_directed(sentence) {
                    return Some(sentence.to_string());
                }
                start = i + ch.len_utf8();
            }
            '.' | '!' | '\n' => start = i + ch.len_utf8(),
            _ => {}
        }
    }
    None
}

/// Whether a question sentence asks *the user* something.
fn is_user_directed(sentence: &str) -> bool {
    let s = sentence
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    USER_DIRECTED_OPENERS.iter().any(|o| s.starts_with(o))
        || s.contains(" you ")
        || s.contains(" you?")
        || s.contains(" your ")
}

/// The alternatives the model enumerated as a `-`/`*`/`N.` list, in order and
/// deduplicated.
fn enumerated_alternatives(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let item = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .map(str::trim)
            .map(str::to_string)
            .or_else(|| numbered_item(line));
        if let Some(item) = item {
            if !item.is_empty() && !out.contains(&item) {
                out.push(item);
            }
        }
    }
    out
}

/// The body of a `1. ` / `1) ` list item, if the line is one.
fn numbered_item(line: &str) -> Option<String> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some(rest.trim().to_string())
}

impl Machine for ReactiveAgentMachine {
    type State = ReactiveState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> ReactiveState {
        ReactiveState::Top
    }

    fn initial(&self) -> ReactiveState {
        ReactiveState::Session
    }

    fn superstate(&self, state: ReactiveState) -> ReactiveState {
        match state {
            ReactiveState::Top => ReactiveState::Top,
            ReactiveState::Session => ReactiveState::Top,
            ReactiveState::Idle | ReactiveState::Generating => ReactiveState::Session,
        }
    }

    fn dispatch_state(
        &mut self,
        state: ReactiveState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<ReactiveState> {
        use ReactiveState::*;

        match state {
            Top => Reaction::Ignored,

            // Session: drills into Idle on Init; handles global cancel.
            Session => match event {
                Event::Internal(InternalEvent::Init) => Reaction::goto(Idle),
                Event::UserCancelled => {
                    Reaction::transition(Idle, vec![], "user cancelled; returning to Idle")
                }
                _ => Reaction::Super(Top),
            },

            // Idle: a user message starts a new turn.
            Idle => match event {
                Event::UserMessage { text } => {
                    init_loop(ctx, CHAT_THREAD, &[], AGENT_MODE, DEFAULT_MAX_TOOL_ROUNDS);
                    Reaction::transition(
                        Generating,
                        vec![Self::first_turn_effect(text)],
                        "user sent message; starting turn",
                    )
                }
                _ => Reaction::Super(Session),
            },

            // Generating: LLM turn in flight; tool loop handled in-state.
            Generating => {
                // ── 1. Delegate tool/approval events to the shared helper ──────
                // Returns Some for ToolSucceeded/ToolFailed/ToolApprovalRequired
                // and for HumanApproved/HumanRejected when a tool approval is
                // in flight.  Returns None otherwise so we can handle LLM events.
                if let Some(r) = handle_tool_event(ctx, Self::continuation_turn, event) {
                    return r;
                }

                // ── 2. LLM and other events ───────────────────────────────────
                match event {
                    Event::LlmTurnComplete { .. } => match on_llm_turn_complete(ctx, event) {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            match Self::clarification_effect(ctx, &text) {
                                // Unresolved: ask, and stay in the loop.
                                Some(ask) => Reaction::effects(vec![ask]),
                                None => {
                                    ctx.set_fact("last_response", json!(text));
                                    Reaction::transition(
                                        Idle,
                                        vec![],
                                        "turn complete; final answer",
                                    )
                                }
                            }
                        }
                        GeneratingAction::CallTools { tool_effects, .. } => {
                            // `on_llm_turn_complete` already registered the pending
                            // calls in LoopState.  Emit CallTool effects and stay.
                            Reaction::effects(tool_effects)
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::effects(vec![nudge_effect])
                        }
                        GeneratingAction::MaxRoundsReached { wrapup_effect } => {
                            Reaction::effects(vec![wrapup_effect])
                        }
                    },

                    Event::LlmFailed { error } => {
                        ctx.set_fact("last_error", json!(error));
                        Reaction::transition(Idle, vec![], "turn failed")
                    }

                    _ => Reaction::Super(Session),
                }
            }
        }
    }

    fn is_terminal(&self, _state: ReactiveState) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{
        dispatch::Hsm,
        effect::EffectKind,
        ids::ToolCallId,
        permissions::ToolCapability,
    };

    fn make_hsm() -> (Hsm<ReactiveAgentMachine>, Context) {
        let mut hsm = Hsm::new(ReactiveAgentMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        (hsm, ctx)
    }

    #[test]
    fn initial_state_is_idle() {
        let (hsm, _) = make_hsm();
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    #[test]
    fn user_message_starts_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
        if let Effect::CallLlm { request } = &out.effects[0] {
            assert_eq!(request["kind"], "turn");
            assert_eq!(request["thread"], CHAT_THREAD);
            assert_eq!(request["instruction"], "fix the bug");
        } else {
            panic!("expected CallLlm turn effect");
        }
    }

    #[test]
    fn llm_turn_complete_with_no_tools_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: "Hello!".to_string(),
                tool_calls: vec![],
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert_eq!(ctx.fact("last_response").unwrap(), &json!("Hello!"));
    }

    #[test]
    fn llm_turn_complete_with_tools_stays_in_generating() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("run tests"), &mut ctx);
        let call_id = ToolCallId::new();
        let out = hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: "shell".to_string(),
                    args: json!({"command": "cargo test"}),
                    capability: ToolCapability::ExecuteShell,
                }],
            },
            &mut ctx,
        );
        // Machine stays in Generating (in-state tool loop).
        assert_eq!(hsm.state(), ReactiveState::Generating);
        // One CallTool effect emitted.
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallTool);
    }

    #[test]
    fn all_tools_done_emits_continuation_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("do work"), &mut ctx);
        let call_id = ToolCallId::new();
        hsm.dispatch(
            &Event::LlmTurnComplete {
                thread: CHAT_THREAD.to_string(),
                text: String::new(),
                tool_calls: vec![sven_hsm::ProposedToolCall {
                    call_id,
                    name: "shell".to_string(),
                    args: json!({}),
                    capability: ToolCapability::ExecuteShell,
                }],
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Generating);
        // Simulate tool success.
        let out = hsm.dispatch(
            &Event::ToolSucceeded {
                call_id,
                observation: json!("ok"),
            },
            &mut ctx,
        );
        // Still in Generating; emitted a continuation turn.
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert_eq!(out.effects.len(), 1);
        assert_eq!(out.effects[0].kind(), EffectKind::CallLlm);
    }

    #[test]
    fn failure_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(
            &Event::LlmFailed {
                error: "boom".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
    }

    #[test]
    fn cancel_during_generation_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("long task"), &mut ctx);
        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    /// A final answer that leaves the request genuinely unresolved — the model
    /// itself asks the user a question and itself enumerates the alternatives.
    const UNRESOLVED_ANSWER: &str = "I can do this two ways.\n\
         Which approach should I take?\n\
         \n\
         - Rewrite the parser in place\n\
         - Add a second parser behind a feature flag\n";

    /// A plain, resolved final answer: no question to the user, no alternatives.
    const UNAMBIGUOUS_ANSWER: &str =
        "Done. I fixed the off-by-one in parser.rs and added a regression test.";

    fn final_answer(text: &str) -> Event {
        Event::LlmTurnComplete {
            thread: CHAT_THREAD.to_string(),
            text: text.to_string(),
            tool_calls: vec![],
        }
    }

    #[test]
    fn a_low_confidence_final_answer_emits_one_ask_question_and_stays_in_generating() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("port the parser"), &mut ctx);
        let out = hsm.dispatch(&final_answer(UNRESOLVED_ANSWER), &mut ctx);

        assert_eq!(
            hsm.state(),
            ReactiveState::Generating,
            "the clarification post-check must keep the loop open"
        );
        assert!(!out.transitioned);
        assert_eq!(out.effects.len(), 1, "exactly one clarification call");
        assert_eq!(out.effects[0].kind(), EffectKind::CallTool);

        let Effect::CallTool {
            call_id,
            name,
            capability,
            args,
        } = &out.effects[0]
        else {
            panic!("expected a CallTool effect");
        };
        assert_eq!(name, "ask_question");
        assert_eq!(*capability, ToolCapability::ReadFile);
        let question = &args["questions"][0];
        assert_eq!(question["prompt"], "Which approach should I take?");
        assert_eq!(
            question["options"],
            json!([
                "Rewrite the parser in place",
                "Add a second parser behind a feature flag"
            ]),
            "the options are the model's own, never invented by the machine"
        );

        // The call must be registered pending, or the loop stalls forever
        // instead of resuming when the user answers.
        let ls = LoopState::load(&ctx);
        assert!(ls.pending.contains(call_id));

        // Transitions must be pure: the same event stream must produce the same
        // call id, or replay never matches the recorded `ToolSucceeded`.
        let (mut replay, mut replay_ctx) = make_hsm();
        replay.dispatch(&Event::user_message("port the parser"), &mut replay_ctx);
        let replayed = replay.dispatch(&final_answer(UNRESOLVED_ANSWER), &mut replay_ctx);
        assert_eq!(replayed.effects, out.effects, "clarification must be pure");
    }

    #[test]
    fn the_clarification_post_check_cannot_exceed_max_tool_rounds() {
        // One round below the cap: the post-check still fires.
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("port the parser"), &mut ctx);
        let mut ls = LoopState::load(&ctx);
        ls.round = ls.max_rounds - 1;
        ls.store(&mut ctx);
        let out = hsm.dispatch(&final_answer(UNRESOLVED_ANSWER), &mut ctx);
        assert_eq!(out.effects.len(), 1, "still inside the round budget");
        assert_eq!(hsm.state(), ReactiveState::Generating);

        // At the cap: the next turn would exceed it, so the post-check is
        // skipped and the machine finishes the turn exactly as it does today.
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("port the parser"), &mut ctx);
        let mut ls = LoopState::load(&ctx);
        ls.round = ls.max_rounds;
        ls.store(&mut ctx);
        let out = hsm.dispatch(&final_answer(UNRESOLVED_ANSWER), &mut ctx);
        assert!(
            out.effects.is_empty(),
            "the post-check must not spend a round past the budget"
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    #[test]
    fn an_unambiguous_final_answer_is_byte_identical_to_todays_behaviour() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("fix the off-by-one"), &mut ctx);
        let out = hsm.dispatch(&final_answer(UNAMBIGUOUS_ANSWER), &mut ctx);

        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert!(out.effects.is_empty());
        assert_eq!(
            ctx.fact("last_response").unwrap(),
            &json!(UNAMBIGUOUS_ANSWER)
        );
        // No pending call was invented behind the answer's back.
        assert!(LoopState::load(&ctx).pending.is_empty());
    }

    #[test]
    fn multiple_turns_loop_through_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        for i in 0..3 {
            hsm.dispatch(&Event::user_message(format!("turn {i}")), &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Generating);
            hsm.dispatch(
                &Event::LlmTurnComplete {
                    thread: CHAT_THREAD.to_string(),
                    text: format!("done {i}"),
                    tool_calls: vec![],
                },
                &mut ctx,
            );
            assert_eq!(hsm.state(), ReactiveState::Idle);
        }
    }
}
