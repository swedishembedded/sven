// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The deliberation-based SDLC machine.
//!
//! [`SdlcMachine`] drives a software-development lifecycle where **every phase
//! is a deliberation**: the HSM issues one comprehensive instruction
//! ([`prompts`]) on that phase's append-only conversation thread with a
//! state-scoped tool subset, and the model returns a structured decision
//! ([`decisions`]) whose `status` the machine maps to a transition.
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
//! ├── Done / Failed / Cancelled  ← terminal
//! ```
//!
//! Each phase handler is self-contained: it issues its deliberation on `Entry`,
//! routes `DeliberationComplete` by decision status, re-deliberates on a
//! developer `UserMessage` (carrying the answer forward append-only), and
//! handles approval replies inline.  This keeps all routing local and avoids a
//! separate await/continuation state machine.

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
    approval_prompt_of, message_of, payload_of, questions_of, status_of, DecisionStatus,
};

/// Maximum number of recovery attempts before giving up.
const MAX_RECOVERY: u32 = 3;

/// States of the deliberation SDLC machine.
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
    Done,
    Failed,
    Cancelled,
}

/// The deliberation-based SDLC machine.
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
    /// verification.  Deliberation tool calls are executed inside the loop, but
    /// the kernel still gates capabilities per state.
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
///
/// A plan decomposes into fan-out work when its payload carries a `tasks`
/// array.  Each entry may be a bare string or an object with a `title`/`task`
/// field.  Returns an empty vec when the plan is single-track.
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

/// Merge the per-child result payloads into a single append-only digest the
/// parent feeds back into the execution thread (cache-safe: a new user turn,
/// never a rewrite of earlier messages).
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

/// `true` when parallel execution is wired (a `ChildSpawner` is installed) and
/// the plan decomposes into ≥2 independent tasks. Gated on the
/// `parallel_execution` fact so a runtime *without* a spawner never emits
/// `InstantiateSubmachine` effects that would otherwise hang (they would
/// no-op).  Bootstrap sets the fact only when it installs a spawner.
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

impl Machine for SdlcMachine {
    type State = SdlcState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> SdlcState {
        SdlcState::Top
    }

    /// Wait for the first developer message before doing anything.
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
            // The machine fires no LLM call until the developer speaks.
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
                    let req = prompts::intake_request(&fact_str(ctx, "user_request"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "scope", decision);
                        Reaction::goto(Discovery)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "scope", decision);
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "intake",
                            prompts::role_for_thread("intake"),
                            prompts::tools_for_thread("intake"),
                            "Continue your investigation and reach a decision.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Intake, "intake failed"),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "intake",
                        prompts::role_for_thread("intake"),
                        prompts::tools_for_thread("intake"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Discovery)
                }
                Event::HumanRejected { .. } => {
                    let req = prompts::revise_request(
                        "intake",
                        prompts::role_for_thread("intake"),
                        prompts::tools_for_thread("intake"),
                        "scope not confirmed",
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Intake, error),
                _ => Reaction::Super(Top),
            },

            // ── Discovery ───────────────────────────────────────────────────
            Discovery => match event {
                Event::Internal(InternalEvent::Entry) => {
                    let req = prompts::discovery_request(&fact_str(ctx, "scope_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "discovery", decision);
                        Reaction::goto(Planning)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "discovery",
                            prompts::role_for_thread("discovery"),
                            prompts::tools_for_thread("discovery"),
                            "Continue exploring and produce the discovery summary.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Discovery, "discovery failed"),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "discovery",
                        prompts::role_for_thread("discovery"),
                        prompts::tools_for_thread("discovery"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
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
                    let req = prompts::planning_request(&fact_str(ctx, "discovery_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "plan", decision);
                        Reaction::goto(Execution)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "plan", decision);
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "planning",
                            prompts::role_for_thread("planning"),
                            prompts::tools_for_thread("planning"),
                            "Continue and produce the plan.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Planning, "planning failed"),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "planning",
                        prompts::role_for_thread("planning"),
                        prompts::tools_for_thread("planning"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Execution)
                }
                Event::HumanRejected { .. } => {
                    let req = prompts::revise_request(
                        "planning",
                        prompts::role_for_thread("planning"),
                        prompts::tools_for_thread("planning"),
                        "plan not approved",
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Planning, error),
                _ => Reaction::Super(Top),
            },

            // ── Execution ───────────────────────────────────────────────────
            Execution => match event {
                Event::Internal(InternalEvent::Entry) => {
                    // Fan-out: if the approved plan decomposes into independent
                    // tasks (and a child spawner is wired), instantiate one
                    // child submachine per task and run them concurrently.
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
                        Reaction::effects(effects)
                    } else {
                        let req = prompts::execution_request(&fact_str(ctx, "plan_summary"));
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                }
                // A fanned-out child task finished: aggregate its result and,
                // once every sibling is in, append a single synthesis turn to
                // the (append-only) execution thread and re-deliberate.
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
                    let req = prompts::followup_request(
                        "execution",
                        prompts::role_for_thread("execution"),
                        prompts::tools_for_thread("execution"),
                        &format!(
                            "All parallel tasks have finished. Their results:\n{merged}\n\n\
                             Integrate them, resolve any conflicts, and confirm the \
                             implementation is complete."
                        ),
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "execution", decision);
                        Reaction::goto(Verification)
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "execution",
                            prompts::role_for_thread("execution"),
                            prompts::tools_for_thread("execution"),
                            "Continue implementing the plan.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Execution, "execution failed"),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "execution",
                        prompts::role_for_thread("execution"),
                        prompts::tools_for_thread("execution"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    let req = prompts::followup_request(
                        "execution",
                        prompts::role_for_thread("execution"),
                        prompts::tools_for_thread("execution"),
                        "Approved. Continue implementing.",
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::HumanRejected { .. } => to_recovery(ctx, Execution, "execution step rejected"),
                Event::LlmFailed { error } => to_recovery(ctx, Execution, error),
                _ => Reaction::Super(Top),
            },

            // ── Verification ────────────────────────────────────────────────
            Verification => match event {
                Event::Internal(InternalEvent::Entry) => {
                    let req = prompts::verification_request(&fact_str(ctx, "execution_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "verification", decision);
                        Reaction::goto(Delivery)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedApproval => {
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "verification",
                            prompts::role_for_thread("verification"),
                            prompts::tools_for_thread("verification"),
                            "Continue verifying and produce the verdict.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => {
                        to_recovery(ctx, Verification, "verification failed")
                    }
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "verification",
                        prompts::role_for_thread("verification"),
                        prompts::tools_for_thread("verification"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
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
                    let req = prompts::delivery_request(&fact_str(ctx, "verification_summary"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        store_phase_result(ctx, "delivery", decision);
                        Reaction::goto(Done)
                    }
                    DecisionStatus::NeedApproval => {
                        store_phase_result(ctx, "delivery", decision);
                        Reaction::effects(vec![request_approval(ctx, decision)])
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    DecisionStatus::NeedTools => {
                        let req = prompts::followup_request(
                            "delivery",
                            prompts::role_for_thread("delivery"),
                            prompts::tools_for_thread("delivery"),
                            "Finalise the delivery summary.",
                        );
                        Reaction::effects(vec![Effect::CallLlm { request: req }])
                    }
                    DecisionStatus::Failed => to_recovery(ctx, Delivery, "delivery failed"),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "delivery",
                        prompts::role_for_thread("delivery"),
                        prompts::tools_for_thread("delivery"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::HumanApproved { approval_id } => {
                    ctx.approve(*approval_id);
                    Reaction::goto(Done)
                }
                Event::HumanRejected { .. } => {
                    let req = prompts::revise_request(
                        "delivery",
                        prompts::role_for_thread("delivery"),
                        prompts::tools_for_thread("delivery"),
                        "delivery not accepted",
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmFailed { error } => to_recovery(ctx, Delivery, error),
                _ => Reaction::Super(Top),
            },

            // ── Recovery ────────────────────────────────────────────────────
            Recovery => match event {
                Event::Internal(InternalEvent::Entry) => {
                    let attempts = ctx.bump_retry("recovery");
                    if attempts > MAX_RECOVERY {
                        return Reaction::goto(Failed);
                    }
                    let req = prompts::recovery_request(&fact_str(ctx, "failure_context"));
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::DeliberationComplete { decision, .. } => match status_of(decision) {
                    DecisionStatus::Proceed => {
                        // Retry the failed phase from scratch.
                        let target = ctx
                            .facts
                            .get("failed_phase")
                            .and_then(Value::as_str)
                            .and_then(state_from_str)
                            .unwrap_or(Intake);
                        Reaction::goto(target)
                    }
                    DecisionStatus::NeedUserInput => {
                        Reaction::effects(vec![ask_user_effect(decision)])
                    }
                    _ => Reaction::goto(Failed),
                },
                Event::UserMessage { text } => {
                    let req = prompts::followup_request(
                        "recovery",
                        "You are Sven diagnosing a failure calmly.",
                        prompts::tools_for_thread("recovery"),
                        text,
                    );
                    Reaction::effects(vec![Effect::CallLlm { request: req }])
                }
                Event::LlmFailed { .. } => Reaction::goto(Failed),
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
        "Done" => Done,
        "Failed" => Failed,
        "Cancelled" => Cancelled,
        _ => return None,
    })
}

#[cfg(test)]
mod tests;
