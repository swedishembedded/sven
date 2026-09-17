// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! `UiTestMachine`: drives a fixed, declared sequence of natural-language UI
//! test steps deterministically - screenshot -> ground -> act -> verify ->
//! next step - with a bounded per-step retry budget. See
//! `.agents/roadmap/android-ui-test.md`'s Phase 3 entry.
//!
//! Implements [`Machine`] directly with its own state list, the same shape
//! [`super::reactive_agent::ReactiveAgentMachine`] uses - **not**
//! [`super::loop_core`], which is built entirely around "ask the LLM what to
//! do next" (`Event::LlmTurnComplete` deciding the next tool call). Here the
//! step sequence is fixed by the test author; the model is used only for one
//! bounded, schema-constrained compile of each step's text (see
//! [`step::compile_step_effect`]) - never to decide what happens next.
//!
//! # State machine
//!
//! ```text
//! Top
//! ├── Seeding    ← parses the first UserMessage as the step list
//! ├── Compiling  ← one schema-constrained CallLlm compiles the current step
//! ├── Locating   ← (tap only) screenshot, then ground; a solid-black
//! │                FLAG_SECURE frame hands off to Acting's ask_user path
//! │                instead of ever reaching the grounding model
//! ├── Acting     ← the android tool call (or ask_question) for this step
//! ├── Retrying   ← bounces back into Compiling for the same step (a real
//! │                state, for the audit trail - mirrors VerifiedTaskMachine)
//! ├── Done       ← terminal: every step completed
//! └── Failed     ← terminal: a step exhausted its retry budget
//! ```
//!
//! # Verification (current scope)
//!
//! "Verify" in the roadmap's pipeline sketch is, today, simply the acting
//! tool call's own success/failure: a `ToolSucceeded` for the `android` (or
//! `ask_question`) call advances to the next step, a `ToolFailed` spends a
//! retry. There is no independent post-action re-screenshot/re-ground check
//! yet - see the roadmap's Phase 3 gaps for why that was deliberately left
//! out of this pass.
//!
//! Swedish Embedded AB implements solutions for deterministic on-device
//! Android UI testing for its clients. If your team needs expertise in
//! HSM-driven test automation or human-in-the-loop hand-off on screens an
//! automated agent must never touch, you can procure our services by sending
//! an email to info@swedishembedded.com.

pub mod step;
pub mod vars;

use std::collections::BTreeMap;

use serde::Deserialize;
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

use step::CompiledStep;

/// Attempts allowed per step before it counts as failed for good.
const MAX_ATTEMPTS_PER_STEP: u32 = 3;

/// Options offered on the secure-screen (`FLAG_SECURE`) hand-off confirmation.
const HANDOFF_OPTIONS: &[&str] = &["Done", "Cancel"];
/// Options offered on a genuine `ask_user` step with no options of its own -
/// `ask_question` requires at least two; a human answers for real via its
/// free-form "Other: <text>" path (see `step::extract_answer_text`).
const ASK_USER_OPTIONS: &[&str] = &["Provide the value", "Skip this step"];

const STEPS_FACT: &str = "ui_test_steps";
const INDEX_FACT: &str = "ui_test_index";
const COMPILED_FACT: &str = "ui_test_compiled";
const SCREENSHOT_FACT: &str = "ui_test_screenshot_path";
const LOCATING_PHASE_FACT: &str = "ui_test_locating_phase";
/// Ordered per-step outcome records (`{index, instruction, passed, attempts,
/// error}`), appended to as each step concludes - public so a caller driving
/// this machine to completion (e.g. an agent-dispatch CLI wrapper) can build
/// its own reply shape from the real outcome instead of re-deriving it.
pub const RESULTS_FACT: &str = "ui_test_results";
const PENDING_CALL_FACT: &str = "ui_test_pending_call";
const ASK_BIND_FACT: &str = "ui_test_ask_bind";
const LAST_FAILURE_FACT: &str = "ui_test_last_failure";
/// Set only in [`UiTestState::Failed`]; a run that never reaches `Failed`
/// never sets it.
pub const ERROR_FACT: &str = "ui_test_error";

/// Internal signal name `Retrying` emits from `Entry` to bounce itself back
/// into `Compiling` without violating "entry handlers never transition
/// themselves" - mirrors `verified_task.rs`'s `RETRY_READY_SIGNAL`.
const RETRY_READY_SIGNAL: &str = "ui_test_retry_ready";

/// The seed a `UserMessage` carries into [`UiTestState::Seeding`]: the fixed,
/// declared sequence of natural-language steps, plus any variables already
/// known before the first step compiles.
///
/// `vars` is how a host-dispatched single-step run threads a resolved
/// upstream node's value into this run's `value_ref` resolution without any
/// new plumbing: the dispatcher seeds it from every `params` field besides
/// the instruction text, keyed by field name, and [`UiTestState::Seeding`]
/// binds each one via [`vars::bind`] - the exact mechanism Phase 3 already
/// built for an in-run `ask_user` answer - before compiling step 0.
#[derive(Debug, Clone, Deserialize)]
pub struct UiTestScript {
    pub steps: Vec<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

/// States of the UI-test machine. See the module doc for the diagram.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UiTestState {
    Top,
    Seeding,
    Compiling,
    Locating,
    Acting,
    Retrying,
    Done,
    Failed,
}

/// The UI-test machine itself. See the module doc.
pub struct UiTestMachine {
    id: MachineId,
}

impl UiTestMachine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// Permission policy: read a screenshot, drive the device, ask a human -
    /// nothing else. Neither capability is inherently dangerous (see
    /// `ToolCapability::is_inherently_dangerous`), so a fixed, reviewed test
    /// script can run unattended, e.g. under an automated CI dispatch.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        PermissionPolicy::builder()
            .allow_globally([ToolCapability::ReadFile, ToolCapability::ControlDevice])
            .build()
    }
}

impl Default for UiTestMachine {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Small Context accessors ────────────────────────────────────────────────

fn load_steps(ctx: &Context) -> Vec<String> {
    ctx.fact(STEPS_FACT)
        .and_then(|v| v.as_array())
        .map_or_else(Vec::new, |a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
}

fn load_index(ctx: &Context) -> u32 {
    ctx.fact(INDEX_FACT).and_then(Value::as_u64).unwrap_or(0) as u32
}

fn load_compiled(ctx: &Context) -> Option<CompiledStep> {
    ctx.fact(COMPILED_FACT)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

fn retry_key(index: u32) -> String {
    format!("ui_test_step_{index}")
}

fn attempt_number(ctx: &Context, index: u32) -> u32 {
    ctx.retry_counters
        .get(&retry_key(index))
        .copied()
        .unwrap_or(0)
}

fn set_pending(ctx: &mut Context, call_id: ToolCallId) {
    ctx.set_fact(
        PENDING_CALL_FACT,
        serde_json::to_value(call_id).expect("ToolCallId always serializes"),
    );
}

fn pending_matches(ctx: &Context, call_id: &ToolCallId) -> bool {
    ctx.fact(PENDING_CALL_FACT)
        .cloned()
        .and_then(|v| serde_json::from_value::<ToolCallId>(v).ok())
        .is_some_and(|pending| pending == *call_id)
}

fn append_result(
    ctx: &mut Context,
    index: u32,
    instruction: &str,
    passed: bool,
    error: Option<&str>,
) {
    let mut results = ctx
        .fact(RESULTS_FACT)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    results.push(json!({
        "index": index,
        "instruction": instruction,
        "passed": passed,
        "attempts": attempt_number(ctx, index).max(1),
        "error": error,
    }));
    ctx.set_fact(RESULTS_FACT, Value::Array(results));
}

/// If the run's current (or just-completed) step is an `ask_user` step that
/// named a `bind` variable, the `(bind name, answer)` pair - exactly what a
/// downstream node in an orchestrating host should read from this run's result,
/// per `vars.rs`'s own binding mechanism. `None` for any other verb, or when
/// the step never got as far as binding an answer (e.g. it failed before
/// being answered).
#[must_use]
pub fn ask_user_binding(ctx: &Context) -> Option<(String, String)> {
    let compiled = load_compiled(ctx)?;
    let name = compiled.bind?;
    let value = vars::resolve(ctx, &name)?;
    Some((name, value))
}

// ─── Step transitions ────────────────────────────────────────────────────────

/// Build the `CallLlm` effect that compiles `steps[index]`.
fn begin_compile(ctx: &Context, index: u32) -> Effect {
    let steps = load_steps(ctx);
    let text = steps.get(index as usize).cloned().unwrap_or_default();
    let known = step::known_var_names(&vars::load(ctx));
    step::compile_step_effect(&text, &known)
}

/// Move on to the next step, or finish if `index` was the last one.
fn advance_or_finish(ctx: &mut Context) -> Reaction<UiTestState> {
    let index = load_index(ctx);
    let steps = load_steps(ctx);
    append_result(
        ctx,
        index,
        steps.get(index as usize).map_or("", String::as_str),
        true,
        None,
    );

    let next = index + 1;
    if (next as usize) >= steps.len() {
        return Reaction::transition(UiTestState::Done, [], "all steps completed");
    }
    ctx.set_fact(INDEX_FACT, next);
    let effect = begin_compile(ctx, next);
    Reaction::transition(
        UiTestState::Compiling,
        vec![effect],
        "advancing to the next step",
    )
}

/// A step failed. Spend a retry if the budget allows, otherwise fail the run
/// for good. Mirrors `verified_task.rs::next_after_attempt`'s bounded-retry
/// shape, per-step rather than per-whole-task.
fn fail_or_retry(ctx: &mut Context, reason: impl Into<String>) -> Reaction<UiTestState> {
    let index = load_index(ctx);
    let reason = reason.into();
    let attempts = ctx.bump_retry(retry_key(index));
    if attempts < MAX_ATTEMPTS_PER_STEP {
        ctx.set_fact(LAST_FAILURE_FACT, reason);
        Reaction::transition(UiTestState::Retrying, [], "step failed; retrying")
    } else {
        let steps = load_steps(ctx);
        let instruction = steps
            .get(index as usize)
            .map_or("", String::as_str)
            .to_string();
        append_result(ctx, index, &instruction, false, Some(&reason));
        ctx.set_fact(
            ERROR_FACT,
            format!("step {index} (\"{instruction}\") exhausted {MAX_ATTEMPTS_PER_STEP} attempts: {reason}"),
        );
        Reaction::transition(UiTestState::Failed, [], "step exhausted its retry budget")
    }
}

/// Resolve `Some(value)` when `answer` should be bound under [`ASK_BIND_FACT`].
fn maybe_bind_answer(ctx: &mut Context, answer: &str) {
    if let Some(name) = ctx
        .fact(ASK_BIND_FACT)
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        vars::bind(ctx, &name, &step::normalize_answer(answer));
    }
}

impl Machine for UiTestMachine {
    type State = UiTestState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> UiTestState {
        UiTestState::Top
    }

    fn initial(&self) -> UiTestState {
        UiTestState::Seeding
    }

    fn superstate(&self, _state: UiTestState) -> UiTestState {
        UiTestState::Top
    }

    fn is_terminal(&self, state: UiTestState) -> bool {
        matches!(state, UiTestState::Done | UiTestState::Failed)
    }

    fn all_states(&self) -> Vec<UiTestState> {
        use UiTestState::*;
        vec![Seeding, Compiling, Locating, Acting, Retrying, Done, Failed]
    }

    fn dispatch_state(
        &mut self,
        state: UiTestState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<UiTestState> {
        use UiTestState::*;

        match state {
            Top => Reaction::Ignored,

            // ── Seeding: parse the script exactly once ─────────────────────
            Seeding => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                Event::UserMessage { text } => {
                    let script: UiTestScript = match serde_json::from_str(text) {
                        Ok(s) => s,
                        Err(e) => {
                            ctx.set_fact(ERROR_FACT, format!("malformed test script: {e}"));
                            return Reaction::transition(
                                Failed,
                                [],
                                "test script could not be parsed",
                            );
                        }
                    };
                    if script.steps.is_empty() {
                        ctx.set_fact(ERROR_FACT, "test script has no steps");
                        return Reaction::transition(Failed, [], "empty test script");
                    }
                    ctx.set_fact(
                        STEPS_FACT,
                        Value::Array(script.steps.iter().cloned().map(Value::String).collect()),
                    );
                    ctx.set_fact(INDEX_FACT, 0u32);
                    ctx.set_fact(RESULTS_FACT, Value::Array(Vec::new()));
                    // Bind any variables already known before step 0 even
                    // compiles (see `UiTestScript::vars`'s own doc) - must
                    // happen before `begin_compile` so the step compiler's
                    // `known_var_names` hint already includes them.
                    for (name, value) in &script.vars {
                        vars::bind(ctx, name, value);
                    }
                    let effect = begin_compile(ctx, 0);
                    Reaction::transition(Compiling, vec![effect], "script seeded; compiling step 0")
                }
                _ => Reaction::Ignored,
            },

            // ── Compiling: one bounded, schema-constrained LLM call ────────
            Compiling => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                Event::LlmTurnComplete { text, .. } => {
                    let index = load_index(ctx);
                    let attempt = attempt_number(ctx, index);
                    match step::parse_compiled_step(text) {
                        Ok(compiled) => {
                            ctx.set_fact(COMPILED_FACT, serde_json::to_value(&compiled).unwrap());
                            dispatch_compiled(ctx, &compiled, index, attempt)
                        }
                        Err(reason) => fail_or_retry(ctx, format!("step compiler: {reason}")),
                    }
                }
                Event::LlmFailed { error } => {
                    fail_or_retry(ctx, format!("step-compiler call failed: {error}"))
                }
                _ => Reaction::Ignored,
            },

            // ── Locating: screenshot, then ground (tap steps only) ─────────
            Locating => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                Event::ToolSucceeded {
                    call_id,
                    observation,
                } => {
                    if !pending_matches(ctx, call_id) {
                        return Reaction::Ignored;
                    }
                    handle_locating_success(ctx, observation)
                }
                Event::ToolFailed { call_id, error } => {
                    if !pending_matches(ctx, call_id) {
                        return Reaction::Ignored;
                    }
                    fail_or_retry(ctx, error.clone())
                }
                _ => Reaction::Ignored,
            },

            // ── Acting: the android/ask_question call for this step ────────
            Acting => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                Event::ToolSucceeded {
                    call_id,
                    observation,
                } => {
                    if !pending_matches(ctx, call_id) {
                        return Reaction::Ignored;
                    }
                    if let Some(text) = observation.as_str() {
                        maybe_bind_answer(ctx, text);
                    }
                    advance_or_finish(ctx)
                }
                Event::ToolFailed { call_id, error } => {
                    if !pending_matches(ctx, call_id) {
                        return Reaction::Ignored;
                    }
                    fail_or_retry(ctx, error.clone())
                }
                Event::QuestionAsked {
                    call_id,
                    prompt,
                    options,
                } => {
                    if !pending_matches(ctx, call_id) {
                        return Reaction::Ignored;
                    }
                    let question_id = sven_hsm::ids::QuestionId::from_uuid(call_id.as_uuid());
                    ctx.set_pending_question(sven_hsm::context::PendingQuestion {
                        question_id,
                        call_id: *call_id,
                        prompt: prompt.clone(),
                        options: options.clone(),
                    });
                    Reaction::effects(vec![Effect::RequestHumanAnswer {
                        question_id,
                        call_id: *call_id,
                        prompt: prompt.clone(),
                        options: options.clone(),
                    }])
                }
                Event::HumanAnswered {
                    question_id,
                    answer,
                } => match ctx.resolve_question(*question_id) {
                    Some(_) => {
                        maybe_bind_answer(ctx, answer);
                        advance_or_finish(ctx)
                    }
                    None => Reaction::Ignored,
                },
                _ => Reaction::Ignored,
            },

            // ── Retrying: a real, audited bounce back into Compiling ───────
            Retrying => match event {
                Event::Internal(InternalEvent::Entry) => {
                    Reaction::effects(vec![Effect::EmitInternal {
                        name: RETRY_READY_SIGNAL.to_string(),
                        payload: Value::Null,
                    }])
                }
                Event::Internal(InternalEvent::Custom { name, .. })
                    if name == RETRY_READY_SIGNAL =>
                {
                    let index = load_index(ctx);
                    let effect = begin_compile(ctx, index);
                    Reaction::transition(
                        Compiling,
                        vec![effect],
                        "starting next attempt for this step",
                    )
                }
                _ => Reaction::Ignored,
            },

            Done | Failed => Reaction::Ignored,
        }
    }
}

/// Route a freshly compiled step to whichever phase it needs: `Locating`
/// for `tap`, straight into `Acting` for everything else (`ask_user` builds
/// its own `CallTool` directly; every other verb goes through
/// `direct_action_effect`).
fn dispatch_compiled(
    ctx: &mut Context,
    compiled: &CompiledStep,
    index: u32,
    attempt: u32,
) -> Reaction<UiTestState> {
    use step::StepVerb;

    match compiled.verb {
        StepVerb::Tap => {
            if compiled.target.is_none() {
                return fail_or_retry(ctx, "tap step compiled with no target");
            }
            let (effect, call_id) = step::screenshot_effect(index, attempt);
            set_pending(ctx, call_id);
            ctx.set_fact(LOCATING_PHASE_FACT, "screenshot");
            Reaction::transition(
                UiTestState::Locating,
                vec![effect],
                "compiled a tap step; locating on screen",
            )
        }
        StepVerb::AskUser => {
            let question = compiled
                .target
                .clone()
                .unwrap_or_else(|| "Please help with this step.".to_string());
            let options = ASK_USER_OPTIONS.iter().map(|s| s.to_string()).collect();
            let (effect, call_id) = step::ask_user_effect(index, attempt, &question, options);
            set_pending(ctx, call_id);
            ctx.set_fact(ASK_BIND_FACT, json!(compiled.bind));
            Reaction::transition(
                UiTestState::Acting,
                vec![effect],
                "compiled an ask_user step",
            )
        }
        _ => match step::direct_action_effect(ctx, compiled, index, attempt) {
            Ok((effect, call_id)) => {
                set_pending(ctx, call_id);
                ctx.set_fact(ASK_BIND_FACT, Value::Null);
                Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "compiled a direct action step",
                )
            }
            Err(reason) => fail_or_retry(ctx, reason),
        },
    }
}

/// Handle a `ToolSucceeded` while in `Locating`: either the screenshot just
/// came back (take the path, ground it) or the ground result just came back
/// (act on it, or hand off to a human on a `FLAG_SECURE` frame).
fn handle_locating_success(ctx: &mut Context, observation: &Value) -> Reaction<UiTestState> {
    let index = load_index(ctx);
    let attempt = attempt_number(ctx, index);
    let phase = ctx
        .fact(LOCATING_PHASE_FACT)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    match phase.as_str() {
        "screenshot" => {
            let text = observation.as_str().unwrap_or("");
            match step::extract_screenshot_path(text) {
                Some(path) => {
                    ctx.set_fact(SCREENSHOT_FACT, path.clone());
                    let target = load_compiled(ctx)
                        .and_then(|c| c.target)
                        .unwrap_or_default();
                    let (effect, call_id) = step::ground_effect(index, attempt, &path, &target);
                    set_pending(ctx, call_id);
                    ctx.set_fact(LOCATING_PHASE_FACT, "ground");
                    Reaction::effects(vec![effect])
                }
                None => fail_or_retry(
                    ctx,
                    format!("could not parse a screenshot path from '{text}'"),
                ),
            }
        }
        "ground" => match step::parse_ground_result(observation) {
            Ok(g) if g.secure_screen => {
                let steps = load_steps(ctx);
                let instruction = steps.get(index as usize).map_or("", String::as_str);
                let question = format!(
                    "This step needs a human: the screen is marked FLAG_SECURE, so a \
                     grounding model cannot see it. Original instruction: \"{instruction}\". \
                     Please complete it manually on the device, then confirm."
                );
                let options = HANDOFF_OPTIONS.iter().map(|s| s.to_string()).collect();
                let (effect, call_id) = step::ask_user_effect(index, attempt, &question, options);
                set_pending(ctx, call_id);
                ctx.set_fact(ASK_BIND_FACT, Value::Null);
                Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "secure screen detected; handing off to a human",
                )
            }
            Ok(g) if g.found && !g.boxes.is_empty() => {
                let (cx, cy) = step::bbox_center(&g.boxes[0].bbox);
                let (effect, call_id) = step::tap_effect(index, attempt, cx, cy);
                set_pending(ctx, call_id);
                Reaction::transition(
                    UiTestState::Acting,
                    vec![effect],
                    "element located; tapping",
                )
            }
            Ok(_) => fail_or_retry(ctx, "target not found on screen"),
            Err(e) => fail_or_retry(ctx, e),
        },
        other => fail_or_retry(
            ctx,
            format!("internal error: unexpected locating phase '{other}'"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sven_hsm::ids::ToolCallId;

    fn make() -> (UiTestMachine, Context, UiTestState) {
        let mut m = UiTestMachine::new();
        let mut ctx = Context::new();
        let state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        (m, ctx, state)
    }

    fn script(steps: &[&str]) -> String {
        json!({ "steps": steps }).to_string()
    }

    fn script_with_vars(steps: &[&str], vars: Value) -> String {
        json!({ "steps": steps, "vars": vars }).to_string()
    }

    /// Effects carried by a `Reaction`, whether it transitioned or just
    /// stayed put emitting some (`dispatch_state` has no separate "outcome"
    /// wrapper the way `Hsm::dispatch` does - see `reactive_agent.rs`'s tests
    /// for that richer, `Hsm`-level shape this crate also uses elsewhere).
    fn effects_of(r: &Reaction<UiTestState>) -> &[Effect] {
        match r {
            Reaction::Transition { effects, .. } | Reaction::Handled(effects) => effects,
            _ => &[],
        }
    }

    /// Drives one event, following a resulting transition's `Entry` handler -
    /// and, since `Entry` handlers may never transition themselves,
    /// `Retrying`'s one self-addressed `EmitInternal`/`Custom` round trip.
    /// Mirrors `verified_task.rs`'s identical test helper.
    fn drive(
        m: &mut UiTestMachine,
        ctx: &mut Context,
        state: &mut UiTestState,
        event: Event,
    ) -> Reaction<UiTestState> {
        let reaction = m.dispatch_state(*state, &event, ctx);
        if let Reaction::Transition { target, .. } = &reaction {
            *state = *target;
            let entry = m.dispatch_state(*state, &Event::entry(), ctx);
            assert!(
                !entry.is_transition(),
                "entry handlers must never transition"
            );
            if let Reaction::Handled(effects) = entry {
                for effect in effects {
                    if let Effect::EmitInternal { name, payload } = effect {
                        let bounced = m.dispatch_state(
                            *state,
                            &Event::Internal(InternalEvent::Custom { name, payload }),
                            ctx,
                        );
                        if let Reaction::Transition { target, .. } = bounced {
                            *state = target;
                            let entry = m.dispatch_state(*state, &Event::entry(), ctx);
                            assert!(
                                !entry.is_transition(),
                                "entry handlers must never transition"
                            );
                        }
                    }
                }
            }
        }
        reaction
    }

    fn compiled_llm_turn(compiled: Value) -> Event {
        Event::LlmTurnComplete {
            thread: step::COMPILE_THREAD.to_string(),
            text: compiled.to_string(),
            tool_calls: vec![],
        }
    }

    fn tool_ok(call_id: ToolCallId, observation: Value) -> Event {
        Event::ToolSucceeded {
            call_id,
            observation,
        }
    }

    fn tool_err(call_id: ToolCallId, error: &str) -> Event {
        Event::ToolFailed {
            call_id,
            error: error.to_string(),
        }
    }

    fn pending_call_id(ctx: &Context) -> ToolCallId {
        serde_json::from_value(ctx.fact(PENDING_CALL_FACT).cloned().unwrap()).unwrap()
    }

    // ── Seeding ──────────────────────────────────────────────────────────────

    #[test]
    fn seeding_parses_the_script_and_starts_compiling_step_0() {
        let (mut m, mut ctx, mut state) = make();
        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        assert_eq!(state, UiTestState::Compiling);
        assert!(out.is_transition());
        assert_eq!(load_steps(&ctx), vec!["Launch the demo app".to_string()]);
        assert_eq!(load_index(&ctx), 0);
    }

    #[test]
    fn a_malformed_script_goes_to_failed() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: "not json".to_string(),
            },
        );
        assert_eq!(state, UiTestState::Failed);
        assert!(ctx.fact(ERROR_FACT).is_some());
    }

    #[test]
    fn an_empty_script_goes_to_failed() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage { text: script(&[]) },
        );
        assert_eq!(state, UiTestState::Failed);
    }

    // ── Compiling -> direct action ──────────────────────────────────────────

    #[test]
    fn a_non_tap_step_compiles_straight_into_acting() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );

        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        assert_eq!(state, UiTestState::Acting);
        assert_eq!(effects_of(&out).len(), 1);
        let Effect::CallTool {
            name,
            args,
            capability,
            ..
        } = &effects_of(&out)[0]
        else {
            panic!("expected CallTool")
        };
        assert_eq!(name, "android");
        assert_eq!(*capability, ToolCapability::ControlDevice);
        assert_eq!(args["action"], "launch_app");
        assert_eq!(args["package"], "com.example.demoapp");
    }

    #[test]
    fn a_tap_step_compiles_into_locating_and_takes_a_screenshot() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Click \"log in with password\""]),
            },
        );

        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "tap", "target": "log in with password" })),
        );
        assert_eq!(state, UiTestState::Locating);
        let Effect::CallTool { name, args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(name, "android");
        assert_eq!(args["action"], "screenshot");
    }

    // ── Locating: screenshot -> ground -> tap ───────────────────────────────

    #[test]
    fn locating_grounds_the_screenshot_then_taps_the_found_box_center() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Click \"log in\""]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "tap", "target": "log in" })),
        );
        assert_eq!(state, UiTestState::Locating);

        let screenshot_id = pending_call_id(&ctx);
        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(
                screenshot_id,
                json!("screenshot saved: shots/shot.png (100x200)"),
            ),
        );
        assert_eq!(
            state,
            UiTestState::Locating,
            "still locating; now grounding"
        );
        let Effect::CallTool { name, args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(name, "ground");
        assert_eq!(args["image_path"], "shots/shot.png");
        assert_eq!(args["target"], "log in");

        let ground_id = pending_call_id(&ctx);
        let ground_json = json!({ "found": true, "boxes": [{ "phrase": "log in", "bbox": [0.1, 0.1, 0.3, 0.3] }] }).to_string();
        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(ground_id, json!(ground_json)),
        );
        assert_eq!(state, UiTestState::Acting);
        let Effect::CallTool { name, args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(name, "android");
        assert_eq!(args["action"], "tap");
        assert_eq!(args["x"], 0.2);
        assert_eq!(args["y"], 0.2);
    }

    #[test]
    fn a_secure_screen_hands_off_to_ask_user_instead_of_tapping() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Confirm on the secure screen"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "tap", "target": "confirm" })),
        );
        let screenshot_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(screenshot_id, json!("screenshot saved: shots/shot.png")),
        );
        let ground_id = pending_call_id(&ctx);

        let secure_json = json!({ "found": false, "boxes": [], "secure_screen": true }).to_string();
        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(ground_id, json!(secure_json)),
        );

        assert_eq!(
            state,
            UiTestState::Acting,
            "must hand off, never attempt to tap a screen it never saw"
        );
        let Effect::CallTool { name, args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(name, "ask_question");
        let question = args["questions"][0]["prompt"].as_str().unwrap();
        assert!(
            question.contains("Confirm on the secure screen"),
            "{question}"
        );
        assert_eq!(args["questions"][0]["options"], json!(["Done", "Cancel"]));
    }

    #[test]
    fn a_target_not_found_on_screen_is_a_failure_not_a_crash() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Click \"log in\""]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "tap", "target": "log in" })),
        );
        let screenshot_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(screenshot_id, json!("screenshot saved: shots/shot.png")),
        );
        let ground_id = pending_call_id(&ctx);
        let not_found = json!({ "found": false, "boxes": [] }).to_string();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(ground_id, json!(not_found)),
        );
        assert_eq!(
            state,
            UiTestState::Compiling,
            "one attempt remains in the default budget; Retrying bounces straight back into Compiling"
        );
        assert_eq!(
            ctx.fact(LAST_FAILURE_FACT).unwrap(),
            "target not found on screen"
        );
    }

    // ── Acting -> advance / done ─────────────────────────────────────────────

    #[test]
    fn the_last_step_succeeding_finishes_the_run() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        let call_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(call_id, json!("launched com.example.demoapp")),
        );

        assert_eq!(state, UiTestState::Done);
        let results = ctx.fact(RESULTS_FACT).unwrap().as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["passed"], true);
    }

    #[test]
    fn a_non_last_step_succeeding_advances_to_compiling_the_next_one() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app", "Wait a bit"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        let call_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(call_id, json!("launched com.example.demoapp")),
        );

        assert_eq!(state, UiTestState::Compiling);
        assert_eq!(load_index(&ctx), 1);
    }

    // ── Variable binding: ask_user -> a later step's value_ref ─────────────

    #[test]
    fn an_ask_user_answer_binds_a_variable_a_later_step_can_reference() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Ask the user for the code", "Enter the code"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(
                json!({ "verb": "ask_user", "target": "What is the code?", "bind": "code" }),
            ),
        );
        assert_eq!(state, UiTestState::Acting);
        let ask_id = pending_call_id(&ctx);

        // TUI-style plain answer text (no "Q:/A:" wrapper).
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(ask_id, json!("123456")),
        );
        assert_eq!(
            state,
            UiTestState::Compiling,
            "advances to compile the next step"
        );
        assert_eq!(vars::resolve(&ctx, "code"), Some("123456".to_string()));

        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "type_text", "value_ref": "code" })),
        );
        assert_eq!(state, UiTestState::Acting);
        let Effect::CallTool { args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(args["action"], "type_text");
        assert_eq!(args["text"], "123456");
    }

    #[test]
    fn an_answer_headless_parks_then_resumes_on_human_answered_and_still_binds() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Ask the user for the code"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(
                json!({ "verb": "ask_user", "target": "What is the code?", "bind": "code" }),
            ),
        );
        let ask_id = pending_call_id(&ctx);

        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::QuestionAsked {
                call_id: ask_id,
                prompt: "What is the code?".to_string(),
                options: vec!["Provide the value".into(), "Skip this step".into()],
            },
        );
        assert_eq!(
            state,
            UiTestState::Acting,
            "parking must not move the machine"
        );
        assert_eq!(effects_of(&out).len(), 1);
        assert_eq!(
            effects_of(&out)[0].kind(),
            sven_hsm::effect::EffectKind::RequestHumanAnswer
        );
        assert!(ctx.pending_question.is_some());

        let question_id = ctx.pending_question.as_ref().unwrap().question_id;
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::HumanAnswered {
                question_id,
                answer: "Other: 654321".to_string(),
            },
        );
        assert_eq!(state, UiTestState::Done);
        assert_eq!(vars::resolve(&ctx, "code"), Some("654321".to_string()));
        assert!(ctx.pending_question.is_none());
    }

    // ── Variable binding: seeded from the script itself ─────────────────────

    #[test]
    fn seeding_binds_vars_supplied_in_the_script_before_compiling_step_0() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script_with_vars(&["Enter the code"], json!({ "code": "999111" })),
            },
        );
        assert_eq!(state, UiTestState::Compiling);
        assert_eq!(
            vars::resolve(&ctx, "code"),
            Some("999111".to_string()),
            "a var supplied in the seed script must be bound before step 0 compiles"
        );

        let out = drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "type_text", "value_ref": "code" })),
        );
        assert_eq!(state, UiTestState::Acting);
        let Effect::CallTool { args, .. } = &effects_of(&out)[0] else {
            panic!("expected CallTool")
        };
        assert_eq!(
            args["text"], "999111",
            "a seeded var resolves through value_ref exactly like an in-run ask_user answer"
        );
    }

    #[test]
    fn a_script_with_no_vars_field_still_seeds_normally() {
        // `vars` is `#[serde(default)]` - a plain `{"steps": [...]}` (every
        // existing script in this test module, and every real pre-this-change
        // caller) must still parse and seed with no bindings.
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        assert_eq!(state, UiTestState::Compiling);
        assert!(vars::load(&ctx).is_empty());
    }

    // ── ask_user_binding: the (name, answer) pair a downstream node reads ──

    #[test]
    fn ask_user_binding_returns_none_before_any_step_runs() {
        let ctx = Context::new();
        assert_eq!(ask_user_binding(&ctx), None);
    }

    #[test]
    fn ask_user_binding_returns_none_for_a_non_ask_user_step() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        assert_eq!(ask_user_binding(&ctx), None);
    }

    #[test]
    fn ask_user_binding_returns_the_bound_name_and_answer_once_the_step_succeeds() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Ask the user for the code"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(
                json!({ "verb": "ask_user", "target": "What is the code?", "bind": "code" }),
            ),
        );
        let ask_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(ask_id, json!("123456")),
        );

        assert_eq!(state, UiTestState::Done);
        assert_eq!(
            ask_user_binding(&ctx),
            Some(("code".to_string(), "123456".to_string()))
        );
    }

    // ── Retry budget ─────────────────────────────────────────────────────────

    #[test]
    fn a_failing_step_retries_up_to_the_budget_then_fails_the_run() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );

        for _ in 0..MAX_ATTEMPTS_PER_STEP {
            drive(
                &mut m,
                &mut ctx,
                &mut state,
                compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
            );
            assert_eq!(state, UiTestState::Acting);
            let call_id = pending_call_id(&ctx);
            drive(
                &mut m,
                &mut ctx,
                &mut state,
                tool_err(call_id, "device not found"),
            );
        }

        assert_eq!(state, UiTestState::Failed);
        let err = ctx.fact(ERROR_FACT).unwrap().as_str().unwrap();
        assert!(err.contains("device not found"), "{err}");
        let results = ctx.fact(RESULTS_FACT).unwrap().as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["passed"], false);
        assert_eq!(results[0]["attempts"], MAX_ATTEMPTS_PER_STEP);
    }

    #[test]
    fn a_step_that_fails_once_then_succeeds_advances_normally() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        let call_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_err(call_id, "transient adb error"),
        );
        assert_eq!(
            state,
            UiTestState::Compiling,
            "bounced back through Retrying into Compiling"
        );

        drive(
            &mut m,
            &mut ctx,
            &mut state,
            compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
        );
        let call_id = pending_call_id(&ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            tool_ok(call_id, json!("launched com.example.demoapp")),
        );
        assert_eq!(state, UiTestState::Done);
    }

    #[test]
    fn a_step_compiler_parse_failure_spends_a_retry_instead_of_crashing() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::LlmTurnComplete {
                thread: step::COMPILE_THREAD.to_string(),
                text: "not json".to_string(),
                tool_calls: vec![],
            },
        );
        assert_eq!(state, UiTestState::Compiling, "retried back into Compiling");
    }

    #[test]
    fn an_llm_failure_spends_a_retry_rather_than_wedging() {
        let (mut m, mut ctx, mut state) = make();
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: script(&["Launch the demo app"]),
            },
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::LlmFailed {
                error: "model unreachable".to_string(),
            },
        );
        assert_eq!(state, UiTestState::Compiling);
    }

    #[test]
    fn replaying_the_same_event_sequence_yields_the_same_call_ids() {
        let run = || {
            let (mut m, mut ctx, mut state) = make();
            drive(
                &mut m,
                &mut ctx,
                &mut state,
                Event::UserMessage {
                    text: script(&["Launch the demo app"]),
                },
            );
            drive(
                &mut m,
                &mut ctx,
                &mut state,
                compiled_llm_turn(json!({ "verb": "launch_app", "target": "com.example.demoapp" })),
            );
            pending_call_id(&ctx)
        };
        assert_eq!(run(), run());
    }
}
