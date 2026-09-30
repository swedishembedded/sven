// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The kernel-native SDLC machine.
//!
//! [`SdlcMachine`] drives a software-development lifecycle where **every phase
//! is a kernel-mediated LLM turn**: the HSM issues one comprehensive instruction
//! ([`prompts`]) on that phase's append-only conversation thread with a
//! state-scoped tool subset and a structured decision schema.  The
//! `TurnExecutor` (`sven-executors`) streams a single response; if the model
//! proposes tool calls they are dispatched as `Effect::CallTool` effects
//! (kernel-gated, concurrent) and the machine **stays in the current phase state**
//! (`Reaction::Handled`) while tools execute.  Once the model produces a
//! tool-free response, its text is parsed as a JSON decision ([`decisions`])
//! whose `status` the machine maps to a transition.
//!
//! ```text
//! Top
//! ├── Idle          ← waits for the first UserMessage
//! ├── Intake        ← classify intent; chat/clarify or confirm scope
//! ├── Discovery     ← explore repo (read-only tools)
//! ├── Planning      ← produce + approve a plan
//! ├── Execution     ← implement (write/build tools); fan-out to child tasks
//! ├── Verification  ← independent build/test verification
//! ├── Delivery      ← summarise + final sign-off
//! ├── Recovery      ← diagnose failures and retry/escalate
//! └── Done / Failed / Cancelled  ── terminal
//! ```
//!
//! Each phase **owns its tool loop** via in-state handling: `LlmTurnComplete`
//! fires `CallTool` effects and returns `Reaction::Handled`; `ToolSucceeded` /
//! `ToolFailed` drain the pending set and emit a continuation turn when all are
//! done, again returning `Reaction::Handled`.  This means `CallTool` effects are
//! always permission-gated against the *real* phase (e.g. `Execution`), fixing
//! the latent bug where gating happened against the removed `RunningTools` state.

pub mod decisions;
pub mod prompts;
pub mod state;
pub mod task;

pub use state::SdlcState;

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

pub(crate) use decisions::parse_sdlc_decision;
use decisions::{
    approval_prompt_of, decision_schema, message_of, payload_of, questions_of, status_of,
    DecisionStatus,
};

use super::loop_core::{
    handle_tool_event, init_loop, on_llm_turn_complete, GeneratingAction, LoopState,
};

/// Maximum number of recovery attempts before giving up.
const MAX_RECOVERY: u32 = 3;

// ─── PhaseSpec table ──────────────────────────────────────────────────────────

/// What happens at the decision level when the developer rejects the phase.
#[derive(Clone, Copy)]
enum OnReject {
    /// Transition to Recovery.
    ToRecovery(&'static str),
    /// Emit a revision follow-up turn (LLM revises and continues in this phase).
    Revise(&'static str),
}

/// Declarative per-phase configuration used by the generic dispatcher.
struct PhaseSpec {
    /// The conversation thread id for this phase.
    thread: &'static str,
    /// Tool names available in this phase.
    tools: &'static [&'static str],
    /// Maximum tool-call rounds before a wrap-up nudge.
    max_rounds: u32,
    /// Context fact key holding the input summary passed to the prompt fn.
    input_key: &'static str,
    /// Build the initial instruction for this phase.
    prompt_fn: fn(&str) -> Value,
    /// Context fact key under which to store the phase result.
    result_key: &'static str,
    /// Where to advance on `proceed` or decision-level `HumanApproved`.
    proceed_to: SdlcState,
    /// Name of this phase for recovery messages.
    phase_name: &'static str,
    /// What to do when the developer rejects at decision level.
    on_reject: OnReject,
}

/// Look up the [`PhaseSpec`] for a standard phase state.
///
/// Returns `None` for `Execution` (custom fan-out) and `Recovery` (custom).
fn phase_spec(state: SdlcState) -> Option<&'static PhaseSpec> {
    use SdlcState::*;
    match state {
        Intake => Some(&PhaseSpec {
            thread: "intake",
            tools: prompts::READ_TOOLS,
            max_rounds: 6,
            input_key: "user_request",
            prompt_fn: |s| prompts::intake_request(s),
            result_key: "scope",
            proceed_to: Discovery,
            phase_name: "intake",
            on_reject: OnReject::Revise(
                "The developer did NOT approve. Revise your approach and continue.",
            ),
        }),
        Discovery => Some(&PhaseSpec {
            thread: "discovery",
            tools: prompts::READ_TOOLS,
            max_rounds: 20,
            input_key: "scope_summary",
            prompt_fn: |s| prompts::discovery_request(s),
            result_key: "discovery",
            proceed_to: Planning,
            phase_name: "discovery",
            on_reject: OnReject::ToRecovery("discovery rejected"),
        }),
        Planning => Some(&PhaseSpec {
            thread: "planning",
            tools: prompts::READ_TOOLS,
            max_rounds: 16,
            input_key: "discovery_summary",
            prompt_fn: |s| prompts::planning_request(s),
            result_key: "plan",
            proceed_to: Execution,
            phase_name: "planning",
            on_reject: OnReject::Revise(
                "The developer did NOT approve the plan. Revise and continue.",
            ),
        }),
        Verification => Some(&PhaseSpec {
            thread: "verification",
            tools: prompts::BUILD_TOOLS,
            max_rounds: 20,
            input_key: "execution_summary",
            prompt_fn: |s| prompts::verification_request(s),
            result_key: "verification",
            proceed_to: Delivery,
            phase_name: "verification",
            on_reject: OnReject::ToRecovery("verification rejected"),
        }),
        Delivery => Some(&PhaseSpec {
            thread: "delivery",
            tools: prompts::READ_TOOLS,
            max_rounds: 12,
            input_key: "verification_summary",
            prompt_fn: |s| prompts::delivery_request(s),
            result_key: "delivery",
            proceed_to: Done,
            phase_name: "delivery",
            on_reject: OnReject::Revise("Delivery not accepted. Revise and continue."),
        }),
        _ => None,
    }
}

// ─── Machine ──────────────────────────────────────────────────────────────────

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
        Self {
            id: MachineId::new(),
        }
    }

    /// Permission policy: kernel-gated per state - reads and provenance-gated
    /// assimilation everywhere; writes/shell during execution and verification.
    ///
    /// Because each phase now owns its tool loop (no more shared `RunningTools`),
    /// `CallTool` effects are always gated against the *real* phase state,
    /// so this policy works correctly — `WriteFile`/`ExecuteShell` in `Execution`
    /// are `Allowed`, not `Forbidden`.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        use SdlcState::{Delivery, Discovery, Execution, Planning, Verification};
        use ToolCapability::{
            AssimilateKnowledge, ExecuteShell, GitOperation, ReadFile, WriteFile,
        };
        PermissionPolicy::builder()
            .allow_globally([
                ReadFile,
                AssimilateKnowledge,
                ToolCapability::IngestDocument,
            ])
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
    // Replay-deterministic: `Context::approve` no-ops on an id mismatch, so a
    // fresh id would silently skip the grant while the transition still fired.
    // The high half namespaces these away from `loop_core`'s call-id-derived ids.
    let seq = u64::from(ctx.bump_retry("sdlc_approval_seq"));
    let approval_id = ApprovalId::from_uuid(uuid::Uuid::from_u64_pair(0x5344_4C43_4150_5052, seq));
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
        call: None,
    }
}

/// Read a context fact as a string, defaulting to `"(none)"`.
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
fn to_recovery(
    ctx: &mut Context,
    failed_phase: SdlcState,
    context_note: &str,
) -> Reaction<SdlcState> {
    ctx.set_fact("failed_phase", json!(format!("{failed_phase:?}")));
    ctx.set_fact("failure_context", json!(context_note));
    Reaction::goto(SdlcState::Recovery)
}

/// Build a continuation turn for a phase with the decision schema.
fn phase_continuation_turn(ls: &mut LoopState) -> Effect {
    ls.continuation_turn_with_schema(decision_schema(), "decision")
}

/// Initialise `LoopState` for a phase entry.
fn phase_init_loop(ctx: &mut Context, thread: &str, tools: &[&str], max_tool_rounds: u32) {
    let tools_owned: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
    init_loop(ctx, thread, &tools_owned, "", max_tool_rounds);
}

/// Build a follow-up turn on a phase thread (after a developer message or revision).
fn phase_followup_turn(
    thread: &str,
    tools: &[&str],
    instruction: &str,
    max_tool_rounds: u32,
) -> Effect {
    let tools_owned: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
    super::loop_core::build_turn_effect(
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

// ── Generic phase dispatcher ───────────────────────────────────────────────────

/// Handle a `GeneratingAction` uniformly for any standard phase.
fn handle_phase_action(
    ctx: &mut Context,
    state: SdlcState,
    spec: &PhaseSpec,
    action: GeneratingAction,
) -> Reaction<SdlcState> {
    match action {
        GeneratingAction::FinalAnswer { text, .. } => match parse_sdlc_decision(&text) {
            Some(decision) => match status_of(&decision) {
                DecisionStatus::Proceed => {
                    store_phase_result(ctx, spec.result_key, &decision);
                    Reaction::goto(spec.proceed_to)
                }
                DecisionStatus::NeedApproval => {
                    store_phase_result(ctx, spec.result_key, &decision);
                    Reaction::effects(vec![request_approval(ctx, &decision)])
                }
                DecisionStatus::NeedUserInput => {
                    Reaction::effects(vec![ask_user_effect(&decision)])
                }
                DecisionStatus::NeedTools => {
                    let mut ls = LoopState::load(ctx);
                    Reaction::effects(vec![phase_continuation_turn(&mut ls)])
                }
                DecisionStatus::Failed => {
                    to_recovery(ctx, state, &format!("{} failed", spec.phase_name))
                }
            },
            None => to_recovery(
                ctx,
                state,
                &format!("{}: failed to parse decision", spec.phase_name),
            ),
        },
        // Tool calls: emit effects and **stay** in this phase.
        GeneratingAction::CallTools { tool_effects, .. } => Reaction::effects(tool_effects),
        GeneratingAction::EmptyTurn { nudge_effect } => Reaction::effects(vec![nudge_effect]),
        GeneratingAction::MaxRoundsReached { wrapup_effect } => {
            Reaction::effects(vec![wrapup_effect])
        }
    }
}

/// Dispatch all events for a standard (non-Execution, non-Recovery) phase.
fn dispatch_phase(
    state: SdlcState,
    spec: &PhaseSpec,
    event: &Event,
    ctx: &mut Context,
) -> Reaction<SdlcState> {
    // ── 1. Try the shared tool-loop helper first ─────────────────────────────
    // Returns Some for ToolSucceeded/ToolFailed/ToolApprovalRequired and for
    // HumanApproved/HumanRejected when `awaiting_tool_approval` is set.
    if let Some(r) = handle_tool_event(ctx, phase_continuation_turn, event) {
        return r;
    }

    // ── 2. Phase-specific events ─────────────────────────────────────────────
    match event {
        Event::Internal(InternalEvent::Entry) => {
            phase_init_loop(ctx, spec.thread, spec.tools, spec.max_rounds);
            let input = fact_str(ctx, spec.input_key);
            let req = (spec.prompt_fn)(&input);
            Reaction::effects(vec![Effect::CallLlm { request: req }])
        }

        Event::LlmTurnComplete { .. } => {
            let action = on_llm_turn_complete(ctx, event);
            handle_phase_action(ctx, state, spec, action)
        }

        Event::UserMessage { text } => {
            let instruction = format!(
                "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                prompts::ANSWER_CONTRACT
            );
            Reaction::effects(vec![phase_followup_turn(
                spec.thread,
                spec.tools,
                &instruction,
                spec.max_rounds,
            )])
        }

        // Decision-level approval (not tool-level — those were caught above).
        // The phase result was already stored when `NeedApproval` was returned.
        Event::HumanApproved { approval_id } => {
            ctx.approve(*approval_id);
            Reaction::goto(spec.proceed_to)
        }

        Event::HumanRejected { .. } => match spec.on_reject {
            OnReject::ToRecovery(msg) => to_recovery(ctx, state, msg),
            OnReject::Revise(msg) => {
                let instruction = format!("{msg} {}", prompts::ANSWER_CONTRACT);
                Reaction::effects(vec![phase_followup_turn(
                    spec.thread,
                    spec.tools,
                    &instruction,
                    spec.max_rounds,
                )])
            }
        },

        Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
            to_recovery(ctx, state, error)
        }

        _ => Reaction::Super(SdlcState::Top),
    }
}

// ─── Machine impl ─────────────────────────────────────────────────────────────

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
        state.is_terminal()
    }

    fn all_states(&self) -> Vec<SdlcState> {
        SdlcState::all()
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch_state(
        &mut self,
        state: SdlcState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<SdlcState> {
        use SdlcState::*;

        // Standard phases are fully table-driven.
        if let Some(spec) = phase_spec(state) {
            return dispatch_phase(state, spec, event, ctx);
        }

        match state {
            // ── Root ──────────────────────────────────────────────────────────
            Top => match event {
                Event::UserCancelled => Reaction::goto(Cancelled),
                _ => Reaction::Ignored,
            },

            // ── Idle ──────────────────────────────────────────────────────────
            Idle => match event {
                Event::UserMessage { text } => {
                    ctx.set_fact("user_request", json!(text));
                    Reaction::goto(Intake)
                }
                _ => Reaction::Super(Top),
            },

            // ── Execution ─────────────────────────────────────────────────────
            // Custom: fan-out + parallel child task tracking + in-state tool loop.
            Execution => {
                // Tool-loop events handled in-state (fixes WriteFile permission bug).
                if let Some(r) = handle_tool_event(ctx, phase_continuation_turn, event) {
                    return r;
                }

                match event {
                    Event::Internal(InternalEvent::Entry) => {
                        let plan_payload = ctx
                            .facts
                            .get("plan_payload")
                            .cloned()
                            .unwrap_or(Value::Null);
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
                        Reaction::effects(vec![phase_followup_turn(
                            "execution",
                            prompts::WRITE_TOOLS,
                            &instruction,
                            40,
                        )])
                    }

                    Event::LlmTurnComplete { .. } => {
                        let action = on_llm_turn_complete(ctx, event);
                        let spec = &PhaseSpec {
                            thread: "execution",
                            tools: prompts::WRITE_TOOLS,
                            max_rounds: 40,
                            input_key: "plan_summary",
                            prompt_fn: |s| prompts::execution_request(s),
                            result_key: "execution",
                            proceed_to: Verification,
                            phase_name: "execution",
                            on_reject: OnReject::ToRecovery("execution step rejected"),
                        };
                        handle_phase_action(ctx, Execution, spec, action)
                    }

                    Event::UserMessage { text } => {
                        let instruction = format!(
                            "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue. {}",
                            prompts::ANSWER_CONTRACT
                        );
                        Reaction::effects(vec![phase_followup_turn(
                            "execution",
                            prompts::WRITE_TOOLS,
                            &instruction,
                            40,
                        )])
                    }

                    Event::HumanApproved { approval_id } => {
                        ctx.approve(*approval_id);
                        let instruction = format!(
                            "Approved. Continue implementing. {}",
                            prompts::ANSWER_CONTRACT
                        );
                        Reaction::effects(vec![phase_followup_turn(
                            "execution",
                            prompts::WRITE_TOOLS,
                            &instruction,
                            40,
                        )])
                    }

                    Event::HumanRejected { .. } => {
                        to_recovery(ctx, Execution, "execution step rejected")
                    }
                    Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                        to_recovery(ctx, Execution, error)
                    }
                    _ => Reaction::Super(Top),
                }
            }

            // ── Recovery ──────────────────────────────────────────────────────
            Recovery => {
                if let Some(r) = handle_tool_event(ctx, phase_continuation_turn, event) {
                    return r;
                }

                match event {
                    Event::Internal(InternalEvent::Entry) => {
                        let attempts = ctx.bump_retry("recovery");
                        if attempts > MAX_RECOVERY {
                            return Reaction::goto(Failed);
                        }
                        phase_init_loop(ctx, "recovery", prompts::READ_TOOLS, 12);
                        let req = prompts::recovery_request(&fact_str(ctx, "failure_context"));
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }

                    Event::LlmTurnComplete { .. } => {
                        let action = on_llm_turn_complete(ctx, event);
                        match action {
                            GeneratingAction::FinalAnswer { text, .. } => {
                                match parse_sdlc_decision(&text) {
                                    Some(decision) => match status_of(&decision) {
                                        DecisionStatus::Proceed => {
                                            // Re-enter the failed phase (no state_from_str needed:
                                            // recovery stores the failed_phase string as Debug).
                                            let target = failed_phase_state(ctx);
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
                            GeneratingAction::CallTools { tool_effects, .. } => {
                                Reaction::effects(tool_effects)
                            }
                            GeneratingAction::EmptyTurn { nudge_effect } => {
                                Reaction::effects(vec![nudge_effect])
                            }
                            GeneratingAction::MaxRoundsReached { .. } => Reaction::goto(Failed),
                        }
                    }

                    Event::UserMessage { text } => {
                        let instruction = format!(
                            "The developer responded:\n\n\"{text}\"\n\nIncorporate this and continue diagnosing. {}",
                            prompts::ANSWER_CONTRACT
                        );
                        Reaction::effects(vec![phase_followup_turn(
                            "recovery",
                            prompts::READ_TOOLS,
                            &instruction,
                            12,
                        )])
                    }

                    Event::LlmFailed { .. } | Event::EffectFailed { .. } => Reaction::goto(Failed),
                    _ => Reaction::Super(Top),
                }
            }

            // ── Terminal states ───────────────────────────────────────────────
            Done | Failed | Cancelled => Reaction::Ignored,

            // Already handled above via `phase_spec`; these arms are unreachable
            // but needed for exhaustiveness.
            Intake | Discovery | Planning | Verification | Delivery => {
                unreachable!("phase_spec covers these states")
            }
        }
    }
}

/// Map the stored `failed_phase` debug string back to a state variant.
/// Falls back to `Intake` for unknown/missing values.
fn failed_phase_state(ctx: &Context) -> SdlcState {
    use SdlcState::*;
    ctx.fact("failed_phase")
        .and_then(Value::as_str)
        .and_then(|s| {
            Some(match s {
                "Intake" => Intake,
                "Discovery" => Discovery,
                "Planning" => Planning,
                "Execution" => Execution,
                "Verification" => Verification,
                "Delivery" => Delivery,
                "Recovery" => Recovery,
                _ => return None,
            })
        })
        .unwrap_or(Intake)
}

#[cfg(test)]
mod tests;
