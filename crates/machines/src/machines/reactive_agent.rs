// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The default coding-agent machine.
//!
//! [`ReactiveAgentMachine`] models the full turn lifecycle of a streaming,
//! native-tool-calling coding agent.  It uses the shared [`super::loop_core`]
//! helpers and runs the tool loop **in-state** - there are no separate
//! `RunningTools` or `AwaitingApproval` states.
//!
//! # State hierarchy
//!
//! ```text
//! Top (root)
//! └── Session
//!     ├── Idle        ← waiting for the user's next message
//!     ├── Generating  ← LLM turn in flight; tools executed in-state
//!     ├── Checking    ← a standing goal's check is running
//!     └── Compacting  ← the conversation is being summarised on request
//! ```
//!
//! # A standing goal
//!
//! `Event::Goal` sets, pauses, resumes, clears or checks the session's goal
//! (`super::goal_flow`). While a goal is active, a turn that settles is not
//! the end: the goal policy (`super::goal`) runs its check (`Checking`),
//! starts the next goal turn, or stops it - and only then does the machine
//! come to rest and end the person's turn. A cancelled or failed turn pauses
//! the goal; nothing continues it but the person.
//!
//! # Compaction on request
//!
//! `CompactRequested` at rest emits `Effect::CompactContext` and enters
//! `Compacting` until the turn executor reports `COMPACTION_FINISHED` (or the
//! effect fails, or the user cancels). While a turn is running its tools, or
//! another compaction is running, the request is refused with a reason the
//! executor shows, and the machine does not wait for the turn: the person
//! asked for it now, and "after the turn" is a different request they can
//! make then. (Events are dispatched one at a time, so a request posted
//! while the model is being awaited reaches the machine after the turn's
//! completion event; surfaces check that the session is idle first.)
//!
//! # In-state tool loop
//!
//! On `UserMessage`, the machine initialises `LoopState` and emits a
//! `CallLlm kind="turn"`.  `TurnExecutor` streams the model and posts
//! `LlmTurnComplete`.  If the model proposed tool calls the machine emits
//! `CallTool` effects **and stays in `Generating`** (`Reaction::Handled`).
//! `ToolSucceeded` / `ToolFailed` / `ToolApprovalRequired` / `HumanApproved`
//! (tool) / `HumanRejected` (tool) are all handled by the shared
//! [`handle_tool_event`] helper which
//! drains the pending set, requests human approval, and emits continuation
//! turns - all while staying in `Generating`.
//!
//! A tool-free `LlmTurnComplete` transitions the machine to `Idle`.
//!
//! # The machine ends the person's turn
//!
//! Every turn this machine asks for carries `machine_ends_turn`, so the turn
//! executor never reports `TurnComplete` for it: a reply without tool calls
//! can still be followed by a clarification question or a transient retry,
//! which only the machine knows about. The machine announces `TurnComplete`
//! (`Effect::Announce`) on the transitions that bring it to rest after a
//! turn. A turn the person stops is ended by the machine too, with
//! `Aborted`, exactly once whatever was in flight (a model reply already in,
//! a tool, the goal's check); the executor then only reports what a cut-off
//! reply had said.
//!
//! # The bounded clarification post-check
//!
//! `Generating` has no separate "needs clarification" state - adding one would
//! contradict the shape above. Instead the `FinalAnswer` branch runs a bounded
//! post-check: when the model's own final answer leaves the request unresolved,
//! the machine emits one `ask_question` `CallTool` and **stays** in
//! `Generating`, so the loop resumes with the user's pick instead of dead-ending
//! in `Idle`. It is bounded by the same `DEFAULT_MAX_TOOL_ROUNDS` budget as
//! the tool loop.
//!
//! The check is deliberately evidence-based and never inventive: it fires only
//! when the model **itself** asked the user a question and **itself** listed,
//! *after asking*, the alternatives that answer it (see
//! `clarification_question`). The machine supplies neither.
//! Anything else - a plain answer, a rhetorical question, a bare "I'm not sure"
//! with no alternatives - is left byte-identical to the previous behaviour,
//! because this path is on every turn of the default mode.

use serde_json::json;
use sven_hsm::{
    context::Context,
    effect::{Effect, EffectKind},
    event::{Event, InternalEvent, COMPACTION_FINISHED},
    ids::{MachineId, ToolCallId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

use super::clarification::clarification_question;
use super::goal_flow;
use super::held_messages;
use super::loop_core::{
    build_turn_effect, handle_tool_event, init_loop, on_llm_turn_complete, GeneratingAction,
    LoopState, Steering, STEER_SIGNAL,
};
use super::reactive_goal::{checked, failed, goal_step, goal_turn, person_turn_ended};

/// The conversation thread name used by the reactive agent.
pub const CHAT_THREAD: &str = "chat";

/// Default maximum tool-call rounds before a forced wrap-up turn.
const DEFAULT_MAX_TOOL_ROUNDS: u32 = 16;

/// The context fact the configured round limit arrives under.
/// `RuntimeBuilder` seeds it from `AgentConfig.max_tool_rounds` for every
/// session; a caller may override it with `RuntimeBuilder::with_context_facts`.
/// Without it `DEFAULT_MAX_TOOL_ROUNDS` stands.
pub const MAX_TOOL_ROUNDS_FACT: &str = "agent.max_tool_rounds";

/// The internal signal that continues the conversation: a turn whose model
/// request carries no new instruction, asked on the thread as it stands.
///
/// An embedding host sends this after seeding a session with a recorded
/// conversation and restoring no kernel state: the model then produces the
/// next turn of that conversation instead of a first turn answering a prompt.
/// A user message would add a message the recorded conversation never held,
/// which is why continuation is a signal of its own rather than an empty
/// [`Event::UserMessage`].
pub const CONTINUE_SIGNAL: &str = "agent.continue";

/// Whether the session registers `ask_question`. The clarification
/// post-check turns a closing question into an `ask_question` call only when
/// it does; a session that never registered the tool gets the answer as
/// written. Absent means available, which is what every session had before
/// tool selection existed.
pub const ASK_QUESTION_AVAILABLE_FACT: &str = "agent.ask_question_available";

/// The configured tool-round budget, from the session's context facts.
/// A fact that is absent, not a number, or zero (a zero budget would stall
/// the loop before its first turn) falls back to the built-in default.
fn configured_max_tool_rounds(ctx: &Context) -> u32 {
    ctx.facts
        .get(MAX_TOOL_ROUNDS_FACT)
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(DEFAULT_MAX_TOOL_ROUNDS)
}

/// Default mode for all-tools resolution.
pub(super) const AGENT_MODE: &str = "agent";

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
/// kernel still enforces it - this only declares which bucket the call falls in.
const ASK_QUESTION_CAPABILITY: ToolCapability = ToolCapability::ReadFile;

/// States of the reactive agent machine.
///
/// `Generating` now owns the full tool loop - no separate `RunningTools` or
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
    /// A standing goal's check is running after a goal turn.
    Checking,
    /// The conversation is being compacted on request; a turn cannot start
    /// until the executor reports back.
    Compacting,
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

    /// The permission policy for the general coding agent: global allow for
    /// the common capabilities. Whether a call needs a person's approval is
    /// the session's approval mode, not the machine's.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::WriteFile,
                ToolCapability::NetworkAccess,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
            ])
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
    #[must_use]
    pub fn plan_permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([
                ToolCapability::ReadFile,
                ToolCapability::NetworkAccess,
                ToolCapability::GitOperation,
                ToolCapability::ExecuteShell,
            ])
            .build()
    }

    /// A person's message starts a turn.
    fn person_turn(ctx: &mut Context, text: &str) -> Reaction<ReactiveState> {
        goal_flow::begin_person_turn(ctx);
        let ls = start_loop(ctx);
        Reaction::transition(
            ReactiveState::Generating,
            vec![ls.stamp(Self::first_turn_effect(text))],
            "user sent message; starting turn",
        )
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

    /// A turn whose model request carries no new instruction, asked on the
    /// chat thread as it stands.
    fn continuation_of_thread(max_rounds: u32) -> Effect {
        build_turn_effect(
            CHAT_THREAD,
            &[],
            AGENT_MODE,
            None,
            None,
            None,
            max_rounds,
            None,
            None,
        )
    }

    /// The answer to a compaction request: compact the chat thread, or (with
    /// a `refusal`) say why the request was declined.
    fn compact_effect(refusal: Option<&str>) -> Effect {
        Effect::CompactContext {
            thread: CHAT_THREAD.to_string(),
            refusal: refusal.map(str::to_string),
        }
    }

    /// Build a continuation turn from the current `LoopState`.
    fn continuation_turn(ls: &mut LoopState) -> Effect {
        ls.continuation_turn()
    }

    /// The bounded clarification post-check on the `FinalAnswer` branch.
    ///
    /// Returns the single `ask_question` `CallTool` to emit - registered as
    /// pending so the shared tool loop resumes the turn when the user answers -
    /// or `None` to finish the turn exactly as before.
    fn clarification_effect(ctx: &mut Context, text: &str) -> Option<Effect> {
        let mut ls = LoopState::load(ctx);
        // `on_llm_turn_complete` has already counted this turn. Spending a round
        // past the budget is what the budget exists to prevent, so the check is
        // simply skipped there and the answer stands.
        if ls.round > ls.max_rounds {
            return None;
        }
        if ctx
            .fact(ASK_QUESTION_AVAILABLE_FACT)
            .and_then(serde_json::Value::as_bool)
            == Some(false)
        {
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
            ReactiveState::Idle
            | ReactiveState::Generating
            | ReactiveState::Checking
            | ReactiveState::Compacting => ReactiveState::Session,
        }
    }

    fn dispatch_state(
        &mut self,
        state: ReactiveState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<ReactiveState> {
        use ReactiveState::*;

        // At rest, the messages held while a turn ran come first.
        if state == Idle {
            if let Some(reaction) = held_messages::at_rest(ctx, event, Self::person_turn) {
                return reaction;
            }
        }
        match state {
            Top => Reaction::Ignored,

            // Session: drills into Idle on Init; handles global cancel.
            // Session: drills into Idle on Init; handles global cancel and the
            // goal commands that arrive while the machine is busy.
            Session => match event {
                Event::Internal(InternalEvent::Init) => Reaction::goto(Idle),
                Event::UserCancelled => {
                    let effects: Vec<Effect> = goal_flow::interrupted(ctx, "the turn was stopped")
                        .into_iter()
                        .collect();
                    Reaction::transition(Idle, effects, "user cancelled; returning to Idle")
                }
                // A message sent while the turn runs steers it: kept for the
                // turn's next model request, which carries it.
                Event::Internal(InternalEvent::Custom { name, payload })
                    if name == STEER_SIGNAL =>
                {
                    if let Some(steering) = Steering::from_payload(payload) {
                        let mut ls = LoopState::load(ctx);
                        ls.steering.push(steering);
                        ls.store(ctx);
                    }
                    Reaction::effects(vec![])
                }
                // A message is never dropped while a turn runs: it is held,
                // and answered once the turn ends (a client that took an
                // error for the turn's end sends one).
                Event::UserMessage { text } => {
                    held_messages::hold(ctx, text);
                    Reaction::effects(vec![])
                }
                Event::Goal {
                    command,
                    awaits_answer,
                } => match goal_flow::command(ctx, command, false, *awaits_answer) {
                    goal_flow::Answer::Stay(effects) => Reaction::effects(effects),
                    // A goal turn starts only at rest; a busy answer never asks for one.
                    goal_flow::Answer::Start { announce, .. } => Reaction::effects(vec![announce]),
                },
                _ => Reaction::Super(Top),
            },

            // Idle: a user message starts a new turn.
            Idle => match event {
                Event::CompactRequested => Reaction::transition(
                    Compacting,
                    vec![Self::compact_effect(None)],
                    "compaction requested; summarising the conversation",
                ),
                Event::UserMessage { text } => Self::person_turn(ctx, text),
                // Steering that arrives at rest (the turn ended first) was
                // never delivered: the host sees no `Steered` before the
                // turn's end and sends it as the next message itself.
                Event::Internal(InternalEvent::Custom { name, .. }) if name == STEER_SIGNAL => {
                    Reaction::effects(vec![])
                }
                Event::Notice { notice, start_turn } => {
                    let record = Effect::RecordNotice {
                        thread: CHAT_THREAD.to_string(),
                        notice: notice.clone(),
                    };
                    if !*start_turn {
                        return Reaction::effects(vec![record]);
                    }
                    goal_flow::begin_person_turn(ctx);
                    let ls = start_loop(ctx);
                    Reaction::transition(
                        Generating,
                        vec![
                            record,
                            ls.stamp(Self::continuation_of_thread(ls.max_rounds)),
                        ],
                        "the host asked for an answer to its notice; starting turn",
                    )
                }
                Event::Goal {
                    command,
                    awaits_answer,
                } => match goal_flow::command(ctx, command, true, *awaits_answer) {
                    goal_flow::Answer::Stay(effects) => Reaction::effects(effects),
                    goal_flow::Answer::Start {
                        announce,
                        instruction,
                    } => goal_turn(ctx, announce, &instruction),
                },
                Event::Internal(InternalEvent::Custom { name, .. }) if name == CONTINUE_SIGNAL => {
                    let ls = start_loop(ctx);
                    Reaction::transition(
                        Generating,
                        vec![ls.stamp(Self::continuation_of_thread(ls.max_rounds))],
                        "the conversation continues",
                    )
                }
                _ => Reaction::Super(Session),
            },

            // Compacting: waits for the executor; a second request is refused.
            Compacting => match event {
                Event::Internal(InternalEvent::Custom { name, .. })
                    if name == COMPACTION_FINISHED =>
                {
                    Reaction::transition(Idle, vec![], "compaction finished")
                }
                Event::EffectFailed {
                    kind: EffectKind::CompactContext,
                    error,
                } => {
                    ctx.set_fact("last_error", json!(error));
                    Reaction::transition(Idle, vec![], "compaction failed")
                }
                Event::CompactRequested => Reaction::effects(vec![Self::compact_effect(Some(
                    "the conversation is already being compacted",
                ))]),
                _ => Reaction::Super(Session),
            },

            // Checking: the goal's check runs; its verdict decides what follows.
            // A check that could not be delivered has no verdict.
            Checking => match event {
                Event::UserCancelled => cut(ctx),
                Event::VerificationComplete { verdict } => checked(ctx, verdict),
                Event::EffectFailed {
                    kind: EffectKind::Verify,
                    error,
                } => checked(
                    ctx,
                    &sven_vocab::verify::VerifierVerdict::Unknown {
                        reason: error.clone(),
                    },
                ),
                Event::CompactRequested => Reaction::effects(vec![Self::compact_effect(Some(
                    "the goal's check is running; wait for it to finish, then compact",
                ))]),
                _ => Reaction::Super(Session),
            },

            // Generating: LLM turn in flight; tool loop handled in-state.
            Generating => {
                // ── 1. Delegate tool/approval events to the shared helper ──────
                // Returns Some for ToolSucceeded/ToolFailed/ToolApprovalRequired
                // and for HumanApproved/HumanRejected when a tool approval is
                // in flight.  Returns None otherwise so we can handle LLM events.
                goal_flow::observe_result(ctx, event);
                if let Some(r) = handle_tool_event(ctx, Self::continuation_turn, event) {
                    return r;
                }

                // ── 2. LLM and other events ───────────────────────────────────
                if matches!(event, Event::LlmTurnComplete { .. }) {
                    reset_transient_retries(ctx);
                }
                match event {
                    Event::UserCancelled => cut(ctx),
                    Event::CompactRequested => Reaction::effects(vec![Self::compact_effect(Some(
                        "a turn is running; stop it or wait for it to finish, then compact",
                    ))]),
                    Event::LlmTurnComplete { .. } => match on_llm_turn_complete(ctx, event) {
                        GeneratingAction::FinalAnswer { text, .. } => {
                            match Self::clarification_effect(ctx, &text) {
                                // Unresolved: ask, and stay in the loop.
                                Some(ask) => Reaction::effects(vec![ask]),
                                None => {
                                    ctx.set_fact("last_response", json!(text));
                                    match goal_flow::settled(ctx) {
                                        Some((announce, next)) => goal_step(ctx, announce, next),
                                        None => person_turn_ended(ctx),
                                    }
                                }
                            }
                        }
                        GeneratingAction::CallTools {
                            tool_effects,
                            calls,
                            ..
                        } => {
                            // `on_llm_turn_complete` already registered the pending
                            // calls in LoopState.  Emit CallTool effects and stay.
                            goal_flow::observe_calls(ctx, &calls);
                            Reaction::effects(tool_effects)
                        }
                        GeneratingAction::EmptyTurn { nudge_effect } => {
                            Reaction::effects(vec![nudge_effect])
                        }
                        GeneratingAction::MaxRoundsReached { wrapup_effect } => {
                            Reaction::effects(vec![wrapup_effect])
                        }
                    },

                    Event::LlmFailed { error } if error.starts_with(sven_hsm::TRANSIENT) => {
                        match retry_transient(ctx) {
                            Some(again) => Reaction::effects(vec![again]),
                            None => failed(ctx, error),
                        }
                    }

                    Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                        failed(ctx, error)
                    }

                    _ => Reaction::Super(Session),
                }
            }
        }
    }

    fn is_terminal(&self, _state: ReactiveState) -> bool {
        false
    }

    fn all_states(&self) -> Vec<ReactiveState> {
        // Every state a snapshot can name. `Top` is the implicit root and is
        // never an active leaf, so it is not resumable and not listed.
        vec![
            ReactiveState::Session,
            ReactiveState::Idle,
            ReactiveState::Generating,
            ReactiveState::Checking,
            ReactiveState::Compacting,
        ]
    }
}

/// Starts the tool loop of a new turn, configured for this machine: its
/// round budget from the session, and the end of the turn left to the
/// machine ([`LoopState::machine_ends_turn`]).
pub(super) fn start_loop(ctx: &mut Context) -> LoopState {
    init_loop(
        ctx,
        CHAT_THREAD,
        &[],
        AGENT_MODE,
        configured_max_tool_rounds(ctx),
    );
    let mut ls = LoopState::load(ctx);
    ls.machine_ends_turn = true;
    ls.store(ctx);
    ls
}

/// The announcement that the person's turn is over: the machine is at rest
/// and waits for them.
pub(super) fn turn_ended() -> Effect {
    Effect::Announce {
        event: Box::new(sven_hsm::UiEvent::TurnComplete),
    }
}

/// The person stopped the turn: a goal it worked on pauses, and the turn
/// ends here, once. Its executor only reports what a cut-off reply had
/// said, so the ending never depends on what was in flight (a model reply
/// already in, a tool, the goal's check) when the stop came.
fn cut(ctx: &mut Context) -> Reaction<ReactiveState> {
    let mut effects: Vec<Effect> = goal_flow::interrupted(ctx, "the turn was stopped")
        .into_iter()
        .collect();
    effects.push(Effect::Announce {
        event: Box::new(sven_hsm::UiEvent::Aborted {
            partial_text: String::new(),
        }),
    });
    Reaction::transition(
        ReactiveState::Idle,
        effects,
        "the person stopped the turn; it ends here",
    )
}

/// Times a turn that failed for a reason that may not recur (a dropped
/// connection, a busy server) is asked again before the turn ends, unless the
/// application configures its own.
const TRANSIENT_RETRIES: u64 = 3;
/// The pause before the first repeat, unless configured; each next one is
/// three times longer.
const TRANSIENT_FIRST_DELAY_MS: u64 = 5_000;
/// The context fact the configured repeat budget arrives under
/// (`AgentConfig.transient_retries`). Zero ends the turn at the first
/// transient failure, for an application that retries on its own.
pub const TRANSIENT_RETRIES_BUDGET_FACT: &str = "agent.transient_retries";
/// The context fact the configured first pause, in milliseconds, arrives
/// under (`AgentConfig.transient_retry_delay_ms`).
pub const TRANSIENT_RETRY_DELAY_FACT: &str = "agent.transient_retry_delay_ms";
/// The repeats already made in this turn.
const TRANSIENT_RETRIES_FACT: &str = "transient_retries";

fn transient_budget(ctx: &Context) -> u64 {
    ctx.fact(TRANSIENT_RETRIES_BUDGET_FACT)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(TRANSIENT_RETRIES)
}

fn transient_first_delay_ms(ctx: &Context) -> u64 {
    ctx.fact(TRANSIENT_RETRY_DELAY_FACT)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(TRANSIENT_FIRST_DELAY_MS)
}

fn transient_retries(ctx: &Context) -> u64 {
    ctx.fact(TRANSIENT_RETRIES_FACT)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
}

fn reset_transient_retries(ctx: &mut Context) {
    if transient_retries(ctx) != 0 {
        ctx.set_fact(TRANSIENT_RETRIES_FACT, json!(0));
    }
}

/// The turn asked again after a transient failure, with its backoff, or
/// `None` once the repeats are spent. The thread already holds everything
/// the failed request carried, so the repeat is a continuation.
fn retry_transient(ctx: &mut Context) -> Option<Effect> {
    let done = transient_retries(ctx);
    if done >= transient_budget(ctx) {
        return None;
    }
    let first_delay_ms = transient_first_delay_ms(ctx);
    ctx.set_fact(TRANSIENT_RETRIES_FACT, json!(done + 1));
    let mut ls = LoopState::load(ctx);
    let mut effect = ls.continuation_turn();
    ls.store(ctx);
    if let Effect::CallLlm { request } = &mut effect {
        request["delay_ms"] =
            json!(first_delay_ms.saturating_mul(3u64.saturating_pow(done as u32)));
    }
    Some(effect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::{
        dispatch::Hsm, effect::EffectKind, ids::ToolCallId, permissions::ToolCapability,
    };

    fn make_hsm() -> (Hsm<ReactiveAgentMachine>, Context) {
        let mut hsm = Hsm::new(ReactiveAgentMachine::new());
        let mut ctx = Context::new();
        hsm.init(&mut ctx);
        (hsm, ctx)
    }

    /// The machine's round budget is configured, not hardcoded: the session
    /// context carries the configured limit as a fact, and `init_loop` uses
    /// it. Without this, a caller that raises `max_tool_rounds` (the headless
    /// loop sample does) is silently capped at the built-in default, and one
    /// that lowers it is silently granted more rounds.
    #[test]
    fn the_round_budget_comes_from_the_context_fact_when_present() {
        // No fact: the built-in default applies.
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        assert_eq!(
            LoopState::load(&ctx).max_rounds,
            DEFAULT_MAX_TOOL_ROUNDS,
            "no configured fact, the default stands"
        );

        // A configured fact wins over the default.
        let (mut hsm, mut ctx) = make_hsm();
        ctx.set_fact("agent.max_tool_rounds", json!(60));
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        assert_eq!(
            LoopState::load(&ctx).max_rounds,
            60,
            "the configured limit, not the hardcoded default"
        );

        // Zero would stall the loop instantly; the default stands instead of
        // honouring a nonsensical value.
        let (mut hsm, mut ctx) = make_hsm();
        ctx.set_fact("agent.max_tool_rounds", json!(0));
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        assert_eq!(
            LoopState::load(&ctx).max_rounds,
            DEFAULT_MAX_TOOL_ROUNDS,
            "a zero budget is refused in favour of the default"
        );
    }

    /// The post-check sits on every turn of the default mode, so it fires only
    /// on evidence the model itself supplied: a question it addressed to the
    /// user, and the alternatives it offered as answers to that question.
    ///
    /// A bullet list of work already done is not a set of alternatives, and the
    /// closing "should I ...?" it precedes is a yes/no question with no options
    /// at all. Treating the two as a pair turns the most ordinary final answer
    /// there is into a multiple-choice prompt whose options are unrelated to
    /// its own question, and holds a turn open that should have ended.
    #[test]
    fn a_list_of_work_done_before_a_closing_question_is_not_a_set_of_alternatives() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("fix the parser"), &mut ctx);
        let out = hsm.dispatch(&final_answer(WORK_DONE_THEN_A_CLOSING_QUESTION), &mut ctx);

        assert!(
            out.effects == vec![turn_ended()],
            "a resolved answer must finish the turn exactly as it did before \
             the post-check existed, got {:?}",
            out.effects
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert_eq!(
            ctx.fact("last_response"),
            Some(&json!(WORK_DONE_THEN_A_CLOSING_QUESTION)),
        );
    }

    #[test]
    fn initial_state_is_idle() {
        let (hsm, _) = make_hsm();
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    fn a_notice() -> sven_vocab::Notice {
        sven_vocab::Notice::new("job.finished", "The render is ready.").unwrap()
    }

    /// Spec: a notice at rest is recorded as the host's and starts no turn
    /// unless the host asked for one; while a turn runs it is left alone, so
    /// it cannot land between a tool call and its result.
    #[test]
    fn a_notice_is_recorded_and_starts_a_turn_only_when_asked() {
        let (mut hsm, mut ctx) = make_hsm();
        let quiet = Event::Notice {
            notice: a_notice(),
            start_turn: false,
        };
        let out = hsm.dispatch(&quiet, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(
            matches!(&out.effects[..], [Effect::RecordNotice { thread, notice }]
                if thread == CHAT_THREAD && *notice == a_notice()),
            "{:?}",
            out.effects
        );

        let asked = Event::Notice {
            notice: a_notice(),
            start_turn: true,
        };
        let out = hsm.dispatch(&asked, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        match &out.effects[..] {
            [Effect::RecordNotice { .. }, Effect::CallLlm { request }] => {
                assert_eq!(
                    request["instruction"], "",
                    "the notice is already in the thread"
                );
            }
            other => panic!("expected the notice, then a turn: {other:?}"),
        }

        let out = hsm.dispatch(&quiet, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert!(out.effects.is_empty(), "{:?}", out.effects);
    }

    fn compaction_effect(effects: &[Effect]) -> (&str, Option<&str>) {
        match effects {
            [Effect::CompactContext { thread, refusal }] => (thread, refusal.as_deref()),
            other => panic!("expected one CompactContext, got {other:?}"),
        }
    }

    /// A request to compact at rest asks the executor to compact the chat
    /// thread and waits for it; the executor's report (or its failure, or a
    /// cancel) brings the machine back to rest.
    #[test]
    fn compaction_requested_at_rest_waits_for_the_executor() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(&Event::CompactRequested, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Compacting);
        assert_eq!(compaction_effect(&out.effects), (CHAT_THREAD, None));

        hsm.dispatch(
            &Event::Internal(InternalEvent::Custom {
                name: COMPACTION_FINISHED.to_string(),
                payload: serde_json::Value::Null,
            }),
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);

        for leaving in [
            Event::UserCancelled,
            Event::EffectFailed {
                kind: EffectKind::CompactContext,
                error: "no model".into(),
            },
        ] {
            hsm.dispatch(&Event::CompactRequested, &mut ctx);
            hsm.dispatch(&leaving, &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Idle, "left by {leaving:?}");
        }
    }

    /// A running turn, or a compaction already running, is not interrupted
    /// and nothing is queued: the request is refused with a reason.
    #[test]
    fn compaction_requested_while_busy_is_refused_not_queued() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("fix the bug"), &mut ctx);
        let out = hsm.dispatch(&Event::CompactRequested, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        let (_, refusal) = compaction_effect(&out.effects);
        assert!(refusal.is_some_and(|r| r.contains("turn is running")));

        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::CompactRequested, &mut ctx);
        let out = hsm.dispatch(&Event::CompactRequested, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Compacting);
        assert!(compaction_effect(&out.effects).1.is_some());
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

    /// Ends the running turn; the signal entering Idle posts, if any.
    fn end_turn(hsm: &mut Hsm<ReactiveAgentMachine>, ctx: &mut Context) -> Option<Event> {
        let ended = hsm.dispatch(
            &Event::LlmFailed {
                error: "boom".into(),
            },
            ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        ended.effects.into_iter().find_map(|e| match e {
            Effect::EmitInternal { name, payload } => {
                Some(Event::Internal(InternalEvent::Custom { name, payload }))
            }
            _ => None,
        })
    }

    /// The text the turn `effects` start sends the model, as JSON.
    fn asked(effects: &[Effect]) -> String {
        serde_json::to_string(effects).unwrap()
    }

    /// A message that arrives while a turn runs (a client that took an error
    /// for the end of the turn sent it) is never dropped: it is held, and the
    /// held messages are answered once the turn ends, one turn each, in the
    /// order they came, ahead of one that comes after the turn ended.
    #[test]
    fn a_message_sent_during_a_turn_is_answered_after_it_in_order() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("first"), &mut ctx);
        let held = hsm.dispatch(&Event::user_message("second"), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Generating);
        assert!(held.effects.is_empty(), "held, not answered mid-turn");
        hsm.dispatch(&Event::user_message("third"), &mut ctx);

        let asks_for_second = end_turn(&mut hsm, &mut ctx).expect("a message is held");
        // One sent at rest, before the held ones were asked for, waits behind them.
        let started = hsm.dispatch(&Event::user_message("fourth"), &mut ctx);
        assert!(asked(&started.effects).contains("second"));
        // The signal finds that turn running and changes nothing.
        assert!(hsm.dispatch(&asks_for_second, &mut ctx).effects.is_empty());

        for next in ["third", "fourth"] {
            let signal = end_turn(&mut hsm, &mut ctx).expect("a message is held");
            let started = hsm.dispatch(&signal, &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Generating);
            assert!(asked(&started.effects).contains(next), "{next}");
        }
        assert!(end_turn(&mut hsm, &mut ctx).is_none(), "nothing left held");
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

    /// A dropped connection or a busy server is no reason to end a run that
    /// has done an hour's work: the turn is asked again, after a growing
    /// pause, a bounded number of times.
    #[test]
    fn a_transient_failure_is_asked_again_with_backoff_then_ends() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let transient = Event::LlmFailed {
            error: format!("{}: error sending request", sven_hsm::TRANSIENT),
        };
        let mut delays = Vec::new();
        for _ in 0..TRANSIENT_RETRIES {
            let out = hsm.dispatch(&transient, &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Generating);
            assert_eq!(out.effects.len(), 1);
            let Effect::CallLlm { request } = &out.effects[0] else {
                panic!("expected a model call: {:?}", out.effects);
            };
            delays.push(request["delay_ms"].as_u64().expect("a backoff"));
        }
        assert!(delays.windows(2).all(|w| w[0] < w[1]), "{delays:?}");
        hsm.dispatch(&transient, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    /// The embedding application owns the repeat budget and the pause: a
    /// host that bounds a run by a deadline, or retries on its own, must be
    /// able to shrink or switch off the machine's repeats.
    #[test]
    fn the_configured_transient_budget_and_pause_replace_the_defaults() {
        let transient = Event::LlmFailed {
            error: format!("{}: connection reset", sven_hsm::TRANSIENT),
        };
        let (mut hsm, mut ctx) = make_hsm();
        ctx.set_fact(TRANSIENT_RETRIES_BUDGET_FACT, json!(0));
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        hsm.dispatch(&transient, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle, "no repeat is configured");

        let (mut hsm, mut ctx) = make_hsm();
        ctx.set_fact(TRANSIENT_RETRIES_BUDGET_FACT, json!(1));
        ctx.set_fact(TRANSIENT_RETRY_DELAY_FACT, json!(7));
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let out = hsm.dispatch(&transient, &mut ctx);
        let Effect::CallLlm { request } = &out.effects[0] else {
            panic!("expected a model call: {:?}", out.effects);
        };
        assert_eq!(request["delay_ms"], json!(7));
        hsm.dispatch(&transient, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle, "one repeat, then the end");
    }

    fn steer(text: &str, attachments: &[&str]) -> Event {
        Event::Internal(InternalEvent::Custom {
            name: STEER_SIGNAL.to_string(),
            payload: json!({ "text": text, "attachments": attachments }),
        })
    }

    fn llm_request(effects: &[Effect]) -> Option<serde_json::Value> {
        effects.iter().find_map(|effect| match effect {
            Effect::CallLlm { request } => Some(request.clone()),
            _ => None,
        })
    }

    /// A message sent while the turn runs is delivered to it at its next
    /// safe point: the model request after the tool results, as the person's
    /// own words with their files, never dropped and never sent twice.
    #[test]
    fn a_steering_message_rides_the_next_model_request_of_the_running_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("fix the parser"), &mut ctx);
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
        // While the tool runs, the person sends two messages.
        assert!(hsm
            .dispatch(&steer("use the new lexer", &["/w/lexer.png"]), &mut ctx)
            .effects
            .is_empty());
        hsm.dispatch(&steer("and keep the tests", &[]), &mut ctx);
        assert_eq!(
            hsm.state(),
            ReactiveState::Generating,
            "the turn is not stopped"
        );

        let out = hsm.dispatch(
            &Event::ToolSucceeded {
                call_id,
                observation: json!("ok"),
            },
            &mut ctx,
        );
        let request = llm_request(&out.effects).expect("the next model request");
        assert_eq!(
            request["instruction"],
            json!("use the new lexer\n\nand keep the tests")
        );
        assert_eq!(
            request["origin"],
            json!(super::super::loop_core::STEER_ORIGIN)
        );
        assert_eq!(request["attachments"], json!(["/w/lexer.png"]));
        assert!(
            LoopState::load(&ctx).steering.is_empty(),
            "consumed, so never delivered twice"
        );
    }

    /// One that arrives once the turn has ended is not delivered (the host
    /// sends it as the next message), so it is never answered twice.
    #[test]
    fn a_steering_message_at_rest_starts_nothing() {
        let (mut hsm, mut ctx) = make_hsm();
        let out = hsm.dispatch(&steer("look at this", &["/w/chip.png"]), &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn a_good_turn_resets_the_transient_retries() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        let transient = Event::LlmFailed {
            error: format!("{}: busy", sven_hsm::TRANSIENT),
        };
        for _ in 0..TRANSIENT_RETRIES {
            hsm.dispatch(&transient, &mut ctx);
        }
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
        hsm.dispatch(
            &Event::ToolSucceeded {
                call_id,
                observation: json!("ok"),
            },
            &mut ctx,
        );
        // The budget is whole again: the next failures are asked again too.
        for _ in 0..TRANSIENT_RETRIES {
            hsm.dispatch(&transient, &mut ctx);
            assert_eq!(hsm.state(), ReactiveState::Generating);
        }
    }

    /// A refused or undeliverable effect (an approval request, a
    /// clarification question) ends the turn instead of leaving the agent
    /// generating forever.
    #[test]
    fn a_failed_effect_ends_the_turn() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("hi"), &mut ctx);
        hsm.dispatch(
            &Event::EffectFailed {
                kind: sven_hsm::EffectKind::RequestHumanApproval,
                error: "no user executor is configured".into(),
            },
            &mut ctx,
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(ctx.fact("last_error").is_some());
    }

    #[test]
    fn cancel_during_generation_returns_to_idle() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("long task"), &mut ctx);
        hsm.dispatch(&Event::UserCancelled, &mut ctx);
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    /// A final answer that leaves the request genuinely unresolved - the model
    /// itself asks the user a question and itself enumerates the alternatives.
    const UNRESOLVED_ANSWER: &str = "I can do this two ways.\n\
         Which approach should I take?\n\
         \n\
         - Rewrite the parser in place\n\
         - Add a second parser behind a feature flag\n";

    /// A resolved answer in the single most common shape a coding agent's
    /// final answer takes: what it did, as a list, and a closing yes/no
    /// question. The list enumerates work done, not alternatives to the
    /// question - nothing in it is an answer to "should I run the tests".
    const WORK_DONE_THEN_A_CLOSING_QUESTION: &str = "I fixed the off-by-one in \
         parser.rs. Changes:\n\
         \n\
         - renamed the token enum\n\
         - added a span field\n\
         \n\
         Should I run the full test suite?";

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
            out.effects == vec![turn_ended()],
            "the post-check must not spend a round past the budget"
        );
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    /// A session without `ask_question` gets the answer as written: the
    /// post-check never calls a tool the session does not have.
    #[test]
    fn the_clarification_post_check_needs_the_tool() {
        let (mut hsm, mut ctx) = make_hsm();
        ctx.set_fact(ASK_QUESTION_AVAILABLE_FACT, json!(false));
        hsm.dispatch(&Event::user_message("port the parser"), &mut ctx);
        let out = hsm.dispatch(&final_answer(UNRESOLVED_ANSWER), &mut ctx);
        assert!(out.effects == vec![turn_ended()], "{:?}", out.effects);
        assert_eq!(hsm.state(), ReactiveState::Idle);
    }

    #[test]
    fn an_unambiguous_final_answer_is_byte_identical_to_todays_behaviour() {
        let (mut hsm, mut ctx) = make_hsm();
        hsm.dispatch(&Event::user_message("fix the off-by-one"), &mut ctx);
        let out = hsm.dispatch(&final_answer(UNAMBIGUOUS_ANSWER), &mut ctx);

        assert_eq!(hsm.state(), ReactiveState::Idle);
        assert!(out.transitioned);
        assert!(out.effects == vec![turn_ended()]);
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
