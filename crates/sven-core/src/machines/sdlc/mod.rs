// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel-native SDLC machine.
//!
//! [`SdlcMachine`] drives a software-development lifecycle where **every phase
//! is a kernel-mediated LLM turn**: the HSM issues one comprehensive instruction
//! ([`prompts`]) on that phase's append-only conversation thread with a
//! state-scoped tool subset and a structured decision schema.  The
//! [`TurnExecutor`](sven_executors) streams a single response; if the model
//! proposes tool calls they are dispatched as `Effect::CallTool` effects
//! (kernel-gated, concurrent) and the machine re-prompts after all results
//! arrive.  Once the model produces a final tool-free response, its text is
//! parsed as the JSON decision ([`decisions`]) whose `status` the machine maps
//! to a transition.
//!
//! ```text
//! Top
//! ├── Idle          ← waits for the first UserMessage (the "hi" guard)
//! ├── Intake        ← classify intent; chat/clarify or confirm scope
//! ├── Discovery     ← explore repo (read-only tools)
//! ├── Planning      ← produce + approve a plan
//! ├── Execution     ← implement (write/build tools)
//! ├── Verification  ← independent build/test verification
//! ├── Delivery      ← summarise + final sign-off
//! ├── Recovery      ← diagnose failures and retry/escalate
//! ├── RunningTools  ← parallel tool calls dispatched (shared across phases)
//! ├── AwaitingApproval ← tool-level approval gate (shared across phases)
//! ├── Done / Failed / Cancelled  ← terminal
//! ```
//!
//! # Re-entry guard
//!
//! When `RunningTools` finishes and transitions back to a phase state, the
//! Entry action is suppressed via the `sdlc_in_continuation` context flag so
//! the initial instruction is not repeated.  The continuation turn effect is
//! emitted by `RunningTools` as part of the transition.

pub mod decisions;
pub mod prompts;
pub mod task;

use serde_json::{json, Value};
use sven_hsm::{
    context::{Context, PendingApproval},
    effect::Effect,
    event::{Event, InternalEvent},
    ids::{ApprovalId, MachineId},
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};

use decisions::{
    approval_prompt_of, decision_schema, message_of, payload_of, questions_of, status_of,
    DecisionStatus,
};

use super::loop_core::{
    all_tools_done, build_turn_effect, current_thread, current_tools, init_loop, mark_calls_pending,
    max_rounds, on_llm_turn_complete, on_tool_result, GeneratingAction,
};

/// Maximum number of recovery attempts before giving up.
const MAX_RECOVERY: u32 = 3;

/// States of the kernel-native SDLC machine.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SdlcState {
    Top,
    Idle,
    Intake,
    Discovery,
    Planning,
    Execution,
    Verification,
    Delivery,
    Recovery,
    /// Tool calls dispatched for the current phase; shared across all phases.
    RunningTools,
    /// Tool-level approval gate; shared across all phases.
    AwaitingApproval,
    Done,
    Failed,
    Cancelled,
}

/// The kernel-native SDLC machine.
pub struct SdlcMachine {
    id: MachineId,
}

impl Default for SdlcMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl SdlcMachine {
    /// Create a new instance.
    #[must_use]
    pub fn new() -> Self {
        Self { id: MachineId::new() }
    }

    /// Permission policy: reads everywhere; writes/shell during execution and
    /// verification.  The kernel gates capabilities per state.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        use SdlcState::{Delivery, Discovery, Execution, Planning, Verification};
        use ToolCapability::{ExecuteShell, GitOperation, ReadFile, WriteFile};
        PermissionPolicy::builder()
            .allow_globally([ReadFile])
            .allow_in(Discovery, [GitOperation])
            .allow_in(Planning, [GitOperation])
            .allow_in(Execution, [WriteFile, GitOperation, ExecuteShell])
            .allow_in(Verification, [ExecuteShell])
            .allow_in(Delivery, [GitOperation])
            .build()
    }
}

// ── Phase helpers ──────────────────────────────────────────────────────────────

/// Build an `AskUser` effect from a decision's message + questions.
fn ask_user_effect(decision: &Value) -> Effect {
    let mut prompt = message_of(decision);
    let questions = questions_of(decision);
    if !questions.is_empty() {
        if !prompt.is_empty() {
            prompt.push_str("\n\n");
        }
        prompt.push_str(&questions.join("\n"));
    }
    if prompt.is_empty() {
        prompt = "Could you provide more detail so I can continue?".to_string();
    }
    Effect::AskUser { prompt }
}

/// Record a pending approval in `ctx` and return the matching effect.
fn request_approval(ctx: &mut Context, decision: &Value) -> Effect {
    let approval_id = ApprovalId::new();
    let description = approval_prompt_of(decision);
    ctx.set_pending_approval(PendingApproval {
        approval_id,
        capability: ToolCapability::GitOperation,
        description: description.clone(),
    });
    Effect::RequestHumanApproval {
        approval_id,
        capability: ToolCapability::GitOperation,
        description,
    }
}

/// Read a fact as a string, defaulting to `(none)`.
fn fact_str(ctx: &Context, key: &str) -> String {
    ctx.facts
        .get(key)
        .map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .unwrap_or_else(|| "(none)".to_string())
}

/// Store a phase's decision summary + payload under `key`.
fn store_phase_result(ctx: &mut Context, key: &str, decision: &Value) {
    let summary = message_of(decision);
    ctx.set_fact(format!("{key}_summary"), json!(summary));
    ctx.set_fact(format!("{key}_payload"), payload_of(decision));
}

/// Extract a plan's parallelisable task list from its payload.
fn tasks_of(payload: &Value) -> Vec<String> {
    payload
        .get("tasks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| match t {
                    Value::String(s) => Some(s.clone()),
                    Value::Object(o) => o
                        .get("title")
                        .or_else(|| o.get("task"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Merge the per-child result payloads into a single digest.
fn merge_child_results(results: &[Value]) -> String {
    results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let summary = r
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| r.to_string());
            format!("Task {}: {summary}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `true` when parallel execution is enabled and the plan has ≥2 tasks.
fn should_fan_out(ctx: &Context, tasks: &[String]) -> bool {
    tasks.len() >= 2
        && ctx
            .facts
            .get("parallel_execution")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Transition into Recovery, recording where the failure happened.
fn to_recovery(ctx: &mut Context, failed_phase: SdlcState, context_note: &str) -> Reaction<SdlcState> {
    ctx.set_fact("failed_phase", json!(format!("{failed_phase:?}")));
    ctx.set_fact("failure_context", json!(context_note));
    Reaction::goto(SdlcState::Recovery)
}

/// Parse a raw LLM response text as a JSON decision value.
/// Tolerates code fences and extracts the first `{...}` object on failure.
pub(super) fn parse_sdlc_decision(raw: &str) -> Option<Value> {
    let stripped = sven_llm::strip_code_fences(raw);
    serde_json::from_str::<Value>(stripped).ok().or_else(|| {
        // Fall back to first balanced { } object.
        let start = stripped.find('{')?;
        let bytes = stripped.as_bytes();
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        for (i, &b) in bytes.iter().enumerate().skip(start) {
            if in_str {
                if escaped { escaped = false; } else if b == b'\\' { escaped = true; } else if b == b'"' { in_str = false; }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return serde_json::from_str::<Value>(&stripped[start..=i]).ok();
                    }
                }
                _ => {}
            }
        }
        None
    })
}

/// Build a continuation turn for the current phase (after tool results arrive).
fn sdlc_continuation_turn(ctx: &Context) -> Effect {
    build_turn_effect(
        &current_thread(ctx),
        &current_tools(ctx),
        "",
        None,
        None,
        None,
        max_rounds(ctx) as u32,
        Some(decision_schema()),
        Some("decision"),
    )
}

/// Read the current SDLC phase state from context.
fn current_sdlc_phase_state(ctx: &Context) -> SdlcState {
    ctx.fact("sdlc_current_phase")
        .and_then(Value::as_str)
        .and_then(state_from_str)
        .unwrap_or(SdlcState::Intake)
}

/// Initialise loop_core for a phase entry.
fn phase_init_loop(ctx: &mut Context, thread: &str, tools: &[&str], max_tool_rounds: u32) {
    let tools_owned: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
    init_loop(ctx, thread, &tools_owned, "", max_tool_rounds);
}

/// Build the initial turn effect for a phase.
#[allow(dead_code)]
fn phase_initial_turn(thread: &str, tools: &[&str], instruction: &str, max_tool_rounds: u32) -> Effect {
    let tools_owned: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
    build_turn_effect(
        thread,
        &tools_owned,
        "",
        None,
        Some(instruction),
        None,
        max_tool_rounds,
        Some(decision_schema()),
        Some("decision"),
    )
}

/// Build a follow-up turn on a phase thread (after a developer message or revision).
fn phase_followup_turn(thread: &str, tools: &[&str], instruction: &str, max_tool_rounds: u32) -> Effect {
    let tools_owned: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
    build_turn_effect(
        thread,
        &tools_owned,
        "",
        None,
        Some(instruction),
        None,
        max_tool_rounds,
        Some(decision_schema()),
        Some("decision"),
    )
}

/// `true` if `Entry` should be suppressed because we are re-entering from `RunningTools`.
fn is_continuation(ctx: &mut Context) -> bool {
    if ctx.facts.get("sdlc_in_continuation").and_then(Value::as_bool).unwrap_or(false) {
        ctx.facts.remove("sdlc_in_continuation");
        true
    } else {
        false
    }
}

// ── Per-phase LlmTurnComplete handlers ────────────────────────────────────────

/// Dispatch the result of an `on_llm_turn_complete` call for the Intake phase.
fn intake_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "scope", &decision);
                        Reaction::goto(Discovery)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "scope", &decision);
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Intake, "intake failed"),
                },
                None => to_recovery(ctx, Intake, "intake: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Intake"));
            Reaction::transition(RunningTools, tool_effects, "intake: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn discovery_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "discovery", &decision);
                        Reaction::goto(Planning)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Discovery, "discovery failed"),
                },
                None => to_recovery(ctx, Discovery, "discovery: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Discovery"));
            Reaction::transition(RunningTools, tool_effects, "discovery: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn planning_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "plan", &decision);
                        Reaction::goto(Execution)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "plan", &decision);
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Planning, "planning failed"),
                },
                None => to_recovery(ctx, Planning, "planning: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Planning"));
            Reaction::transition(RunningTools, tool_effects, "planning: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn execution_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "execution", &decision);
                        Reaction::goto(Verification)
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Execution, "execution failed"),
                },
                None => to_recovery(ctx, Execution, "execution: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Execution"));
            Reaction::transition(RunningTools, tool_effects, "execution: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn verification_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "verification", &decision);
                        Reaction::goto(Delivery)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Verification, "verification failed"),
                },
                None => to_recovery(ctx, Verification, "verification: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Verification"));
            Reaction::transition(RunningTools, tool_effects, "verification: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn delivery_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "delivery", &decision);
                        Reaction::goto(Done)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "delivery", &decision);
                        Reaction::effects(vec![request_approval(ctx, &decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    DecisionStatus::NeedTools => {
                        Reaction::effects(vec![sdlc_continuation_turn(ctx)])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Delivery, "delivery failed"),
                },
                None => to_recovery(ctx, Delivery, "delivery: failed to parse decision"),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Delivery"));
            Reaction::transition(RunningTools, tool_effects, "delivery: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => Reaction::effects(vec![wrapup_effect]),
    }
}

fn recovery_handle_action(ctx: &mut Context, action: GeneratingAction) -> Reaction<SdlcState> {
    use SdlcState::*;
    match action {
        GeneratingAction::FinalAnswer { text, .. } => {
            match parse_sdlc_decision(&text) {
                Some(decision) => match status_of(&decision) {
                    DecisionStatus::Proceed => {
                        let target = ctx
                            .facts
                            .get("failed_phase")
                            .and_then(Value::as_str)
                            .and_then(state_from_str)
                            .unwrap_or(Intake);
                        Reaction::goto(target)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(&decision)])
                    }
                    _ => Reaction::goto(Failed),
                },
                None => Reaction::goto(Failed),
            }
        }
        GeneratingAction::CallTools { calls, tool_effects, .. } => {
            mark_calls_pending(ctx, &calls);
            ctx.set_fact("sdlc_current_phase", json!("Recovery"));
            Reaction::transition(RunningTools, tool_effects, "recovery: dispatching tool calls")
        }
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { .. } => Reaction::goto(SdlcState::Failed),
    }
}

impl Machine for SdlcMachine {
    type State = SdlcState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> SdlcState {
        SdlcState::Top
    }

    fn initial(&self) -> SdlcState {
        SdlcState::Idle
    }

    fn superstate(&self, _state: SdlcState) -> SdlcState {
        SdlcState::Top
    }

    fn is_terminal(&self, state: SdlcState) -> bool {
        matches!(state, SdlcState::Done | SdlcState::Failed | SdlcState::Cancelled)
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch_state(
        &mut self,
        state: SdlcState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<SdlcState> {
        use SdlcState::*;

        match state {
            // ── Root ──────────────────────────────────────────────────────
            Top => match event {
                Event::UserCancelled => Reaction::goto(Cancelled),
                _ => Reaction::Ignored,
            },

            // ── Idle: the "hi" guard ───────────────────────────────────────
            Idle => match event {
                Event::UserMessage { text } => {
                    ctx.set_fact("user_request", json!(text));
                    Reaction::goto(Intake)
                }
                _ => Reaction::Super(Top),
            },

            // ── Intake ──────────────────────────────────────────────────────
            Intake => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    phase_init_loop(ctx, "intake", prompts::READ_TOOLS, 6);
                    let req = prompts::intake_request(&fact_str(ctx, "user_request"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); intake_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("intake", prompts::READ_TOOLS, &instruction, 6)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Discovery)
                }
                Event::HumanRejected { .. } => {
                    let instruction = format!(
                        "The developer did NOT approve. Revise your approach and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("intake", prompts::READ_TOOLS, &instruction, 6)])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Intake, error),
                _ => Reaction::Super(Top),
            },

            // ── Discovery ───────────────────────────────────────────────────
            Discovery => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    phase_init_loop(ctx, "discovery", prompts::READ_TOOLS, 20);
                    let req = prompts::discovery_request(&fact_str(ctx, "scope_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); discovery_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("discovery", prompts::READ_TOOLS, &instruction, 20)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Planning)
                }
                Event::HumanRejected { .. } => to_recovery(ctx, Discovery, "discovery rejected"),
                Event::LlmFailed { error } => to_recovery(ctx, Discovery, error),
                _ => Reaction::Super(Top),
            },

            // ── Planning ────────────────────────────────────────────────────
            Planning => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    phase_init_loop(ctx, "planning", prompts::READ_TOOLS, 16);
                    let req = prompts::planning_request(&fact_str(ctx, "discovery_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); planning_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("planning", prompts::READ_TOOLS, &instruction, 16)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Execution)
                }
                Event::HumanRejected { .. } => {
                    let instruction = format!(
                        "The developer did NOT approve the plan. Revise and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("planning", prompts::READ_TOOLS, &instruction, 16)])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Planning, error),
                _ => Reaction::Super(Top),
            },

            // ── Execution ───────────────────────────────────────────────────
            Execution => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    // Fan-out: if the approved plan decomposes into independent tasks
                    // (and a child spawner is wired), instantiate one per task.
                    let plan_payload =
                        ctx.facts.get("plan_payload").cloned().unwrap_or(Value::Null);
                    let tasks = tasks_of(&plan_payload);
                    if should_fan_out(ctx, &tasks) {
                        ctx.set_fact("exec_remaining", json!(tasks.len() as i64));
                        ctx.set_fact("exec_results", json!([]));
                        let effects: Vec<Effect> = tasks
                            .iter()
                            .enumerate()
                            .map(|(i, task)| Effect::InstantiateSubmachine {
                                machine: MachineId::new(),
                                descriptor: json!({ "index": i, "task": task }),
                            })
                            .collect();
                        return Reaction::effects(effects);
                    }
                    phase_init_loop(ctx, "execution", prompts::WRITE_TOOLS, 40);
                    let req = prompts::execution_request(&fact_str(ctx, "plan_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::Internal(InternalEvent::SubmachineCompleted { result, .. }) => {
                    let mut results = ctx
                        .facts
                        .get("exec_results")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    results.push(result.clone());
                    ctx.set_fact("exec_results", json!(results));
                    let remaining = ctx
                        .facts
                        .get("exec_remaining")
                        .and_then(Value::as_i64)
                        .unwrap_or(0)
                        - 1;
                    ctx.set_fact("exec_remaining", json!(remaining));
                    if remaining > 0 {
                        return Reaction::handled();
                    }
                    let merged = merge_child_results(&results);
                    ctx.set_fact("execution_summary", json!(merged));
                    phase_init_loop(ctx, "execution", prompts::WRITE_TOOLS, 40);
                    let instruction = format!(
                        "All parallel tasks have finished. Their results:\n{merged}\n\n\
                         Integrate them, resolve any conflicts, and confirm the \
                         implementation is complete. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("execution", prompts::WRITE_TOOLS, &instruction, 40)])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); execution_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("execution", prompts::WRITE_TOOLS, &instruction, 40)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    let instruction = format!("Approved. Continue implementing. {}", prompts::ANSWER_CONTRACT);
                    Reaction::effects(vec![phase_followup_turn("execution", prompts::WRITE_TOOLS, &instruction, 40)])
                }
                Event::HumanRejected { .. } => to_recovery(ctx, Execution, "execution step rejected"),
                Event::LlmFailed { error } => to_recovery(ctx, Execution, error),
                _ => Reaction::Super(Top),
            },

            // ── Verification ────────────────────────────────────────────────
            Verification => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    phase_init_loop(ctx, "verification", prompts::BUILD_TOOLS, 20);
                    let req = prompts::verification_request(&fact_str(ctx, "execution_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); verification_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("verification", prompts::BUILD_TOOLS, &instruction, 20)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Delivery)
                }
                Event::HumanRejected { .. } => to_recovery(ctx, Verification, "verification rejected"),
                Event::LlmFailed { error } => to_recovery(ctx, Verification, error),
                _ => Reaction::Super(Top),
            },

            // ── Delivery ────────────────────────────────────────────────────
            Delivery => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    phase_init_loop(ctx, "delivery", prompts::READ_TOOLS, 12);
                    let req = prompts::delivery_request(&fact_str(ctx, "verification_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); delivery_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("delivery", prompts::READ_TOOLS, &instruction, 12)])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Done)
                }
                Event::HumanRejected { .. } => {
                    let instruction = format!(
                        "Delivery not accepted. Revise and continue. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("delivery", prompts::READ_TOOLS, &instruction, 12)])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Delivery, error),
                _ => Reaction::Super(Top),
            },

            // ── Recovery ────────────────────────────────────────────────────
            Recovery => match event {
                Event::Internal(InternalEvent::Entry) => {
                    if is_continuation(ctx) { return Reaction::handled(); }
                    let attempts = ctx.bump_retry("recovery");
                    if attempts > MAX_RECOVERY {
                        return Reaction::goto(Failed);
                    }
                    phase_init_loop(ctx, "recovery", prompts::READ_TOOLS, 12);
                    let req = prompts::recovery_request(&fact_str(ctx, "failure_context"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmTurnComplete { .. } => {
                    { let a = on_llm_turn_complete(ctx, event); recovery_handle_action(ctx, a) }
                }
                Event::UserMessage { text } => {
                    let instruction = format!(
                        "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue diagnosing. {}",
                        prompts::ANSWER_CONTRACT
                    );
                    Reaction::effects(vec![phase_followup_turn("recovery", prompts::READ_TOOLS, &instruction, 12)])
                }
                Event::LlmFailed { .. } => Reaction::goto(Failed),
                _ => Reaction::Super(Top),
            },

            // ── RunningTools ─────────────────────────────────────────────────
            RunningTools => match event {
                Event::ToolSucceeded { call_id, .. } | Event::ToolFailed { call_id, .. } => {
                    let all_done = on_tool_result(ctx, call_id);
                    if all_done {
                        let next_turn = sdlc_continuation_turn(ctx);
                        let phase = current_sdlc_phase_state(ctx);
                        ctx.set_fact("sdlc_in_continuation", json!(true));
                        Reaction::transition(phase, vec![next_turn], "all tools done; resuming phase")
                    } else {
                        Reaction::handled()
                    }
                }
                Event::ToolApprovalRequired { call_id, capability, description } => {
                    let _ = on_tool_result(ctx, call_id);
                    ctx.set_fact("approval_pending_call_id", json!(call_id.as_uuid().to_string()));
                    Reaction::transition(
                        AwaitingApproval,
                        vec![Effect::RequestHumanApproval {
                            approval_id: ApprovalId::new(),
                            capability: *capability,
                            description: description.clone(),
                        }],
                        "tool approval required",
                    )
                }
                _ => Reaction::Super(Top),
            },

            // ── AwaitingApproval ─────────────────────────────────────────────
            AwaitingApproval => match event {
                Event::HumanApproved { .. } => {
                    if all_tools_done(ctx) {
                        let next_turn = sdlc_continuation_turn(ctx);
                        let phase = current_sdlc_phase_state(ctx);
                        ctx.set_fact("sdlc_in_continuation", json!(true));
                        Reaction::transition(phase, vec![next_turn], "tool approved; all done; resuming")
                    } else {
                        Reaction::goto(RunningTools)
                    }
                }
                Event::HumanRejected { .. } => {
                    if all_tools_done(ctx) {
                        let next_turn = sdlc_continuation_turn(ctx);
                        let phase = current_sdlc_phase_state(ctx);
                        ctx.set_fact("sdlc_in_continuation", json!(true));
                        Reaction::transition(phase, vec![next_turn], "tool rejected; all done; resuming")
                    } else {
                        Reaction::goto(RunningTools)
                    }
                }
                _ => Reaction::Super(Top),
            },

            // ── Terminal states ─────────────────────────────────────────────
            Done | Failed | Cancelled => Reaction::Ignored,
        }
    }
}

/// Map a state `Debug` label back to its variant (used by recovery routing).
fn state_from_str(s: &str) -> Option<SdlcState> {
    use SdlcState::*;
    Some(match s {
        "Top" => Top,
        "Idle" => Idle,
        "Intake" => Intake,
        "Discovery" => Discovery,
        "Planning" => Planning,
        "Execution" => Execution,
        "Verification" => Verification,
        "Delivery" => Delivery,
        "Recovery" => Recovery,
        "RunningTools" => RunningTools,
        "AwaitingApproval" => AwaitingApproval,
        "Done" => Done,
        "Failed" => Failed,
        "Cancelled" => Cancelled,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
