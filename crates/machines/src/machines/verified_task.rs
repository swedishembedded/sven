// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The verified-task machine: freeze a verifier before attempting, grade
//! out of band, never let a claim substitute for a check.
//!
//! # Why this machine exists
//!
//! Every other machine's "done" comes from parsing the model's own text as a
//! decision (`SdlcMachine`/`TaskMachine` both call `parse_sdlc_decision` on
//! the model's final turn and trust its `status` field). That is
//! self-grading: an agent that says "Done!" is treated as evidence the task
//! is done. [`VerifiedTaskMachine`] never does this. The model's final turn
//! means exactly one thing here - "I believe I am finished" - and nothing
//! else; whether that belief is correct is decided entirely by
//! `Event::VerificationComplete`, which only an executor that actually ran a
//! [`sven_vocab::verify::VerifierSpec`] against the real world can produce.
//! Model text is never parsed as a verdict.
//!
//! # State machine
//!
//! ```text
//! Top
//! ├── Freeze      ← parses the first UserMessage as a VerifiedTaskSeed,
//! │                 pins the verifier's hash, never revisited
//! ├── Attempting  ← delegates wholesale to loop_core's tool loop; a
//! │                 tool-free final turn means "claims done", not "is done"
//! ├── Verifying   ← awaits VerificationComplete; recomputes the spec hash
//! │                 and compares before trusting the verdict
//! ├── Retrying    ← bounces straight back into Attempting for the next
//! │                 attempt (a real state, for the audit trail)
//! ├── Done        ← terminal: passed, exhausted retries, or a setup error
//! └── Parked      ← terminal: the verifier itself could not reach a verdict
//!                   (`NeedsHuman`/`Unknown`) - see the module's deferred-work
//!                   note on why this does not yet route through Stage 1's
//!                   question ledger.
//! ```
//!
//! # The four anti-self-grading guarantees
//!
//! 1. **Freeze-before-attempt.** `Freeze` is the only state that ever writes
//!    [`FROZEN_VERIFIER_FACT`]; every other state only reads it.  No tool
//!    available during `Attempting` can write an arbitrary `Context` fact
//!    (tool results become thread messages, not fact mutations), so nothing
//!    in the attempt loop can rewrite the spec - but `Verifying` still
//!    recomputes and compares the hash before trusting a verdict, as defense
//!    in depth against a future bug reopening that path.
//! 2. **Origin attached, never claimed.** The [`sven_vocab::verify::VerifierOrigin`]
//!    pinned at freeze time names the file bytes the verifier came from; the
//!    model never supplies one.
//! 3. **Grading is out of band.** The only path to a verdict is
//!    `Event::VerificationComplete`, produced by `sven_executors::verify::VerifyExecutor`
//!    actually running the spec. `Attempting`'s final-turn handler reads no
//!    JSON decision from the model at all - contrast `TaskMachine`.
//! 4. **Escalation, not assumption.** A verifier that cannot decide
//!    (`NeedsHuman`/`Unknown`) never becomes a pass; it parks, taking the
//!    outcome to `sven-session-model`'s `Unknown`, never a default success.
//!
//! # Deferred (see the Stage 4 plan entry for the full reasoning)
//!
//! Per-attempt reward is recorded here as a plain passed/failed verdict, not
//! yet graded by tool-error ratio; full `Trajectory.extra.episode` provenance
//! (`agent{}`/`policy{}`/`env{}`) and hot-swap-mid-attempt detection are not
//! wired; `Parked`'s question is recorded as a fact rather than routed
//! through Stage 1's call-id-shaped question ledger, since a verifier verdict
// is not a tool call and forcing it through that primitive would need a new
//! deterministic-id scheme rather than reusing an existing correlation.

use serde_json::{json, Value};
use sven_hsm::{
    context::Context,
    effect::Effect,
    event::{Event, InternalEvent},
    ids::MachineId,
    machine::Machine,
    permissions::{PermissionPolicy, ToolCapability},
    status::Reaction,
};
use sven_vocab::verify::{FrozenVerifier, VerifiedTaskSeed, VerifierVerdict};

use super::loop_core::{
    build_turn_effect, handle_tool_event, init_loop, mark_calls_pending, on_llm_turn_complete,
    GeneratingAction,
};

/// Conversation thread every attempt appends to.
const THREAD: &str = "verified_task";
/// Tool-call rounds allowed per attempt before it counts as exhausted.
const MAX_ROUNDS_PER_ATTEMPT: u32 = 40;

/// Context fact: the [`FrozenVerifier`], as JSON, set once by `Freeze`.
const FROZEN_VERIFIER_FACT: &str = "verified_task_frozen_verifier";
/// Context fact: the task id, set once by `Freeze`.
const TASK_ID_FACT: &str = "verified_task_id";
/// Context fact: the task prompt, set once by `Freeze`.
const TASK_PROMPT_FACT: &str = "verified_task_prompt";
/// Context fact: the retry budget, set once by `Freeze`.
const MAX_ATTEMPTS_FACT: &str = "verified_task_max_attempts";
/// Context fact: 0-based index of the attempt in progress.
const ATTEMPT_INDEX_FACT: &str = "verified_task_attempt_index";
/// Context fact: array of completed attempt records - see [`attempt_record`].
const ATTEMPTS_FACT: &str = "verified_task_attempts";
/// Context fact: the terminal `{"passed": bool}` a surface reads to build a
/// `sven_session_model::Verdict` - absent means "no terminal verdict yet",
/// exactly the contract `SessionOutcome::Unknown` depends on.
pub const VERDICT_FACT: &str = "verified_task_verdict";
/// Context fact: a human-facing reason the run never reached a verdict at
/// all (malformed seed, tampered spec) - distinct from [`VERDICT_FACT`] so a
/// setup failure cannot be misread as a graded failure.
pub const ERROR_FACT: &str = "verified_task_error";
/// Context fact: the question a `NeedsHuman`/`Unknown` verifier verdict
/// could not resolve on its own, set when transitioning to [`Parked`](VerifiedTaskState::Parked).
pub const NEEDS_HUMAN_FACT: &str = "verified_task_needs_human";

/// Internal signal name `Retrying` emits from `Entry` to bounce itself back
/// into `Attempting` without violating "entry handlers never transition".
const RETRY_READY_SIGNAL: &str = "verified_task_retry_ready";

/// States of the verified-task machine. See the module doc for the diagram.
#[allow(missing_docs)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VerifiedTaskState {
    Top,
    Freeze,
    Attempting,
    Verifying,
    Retrying,
    Done,
    Parked,
}

/// The verified-task machine itself. See the module doc.
pub struct VerifiedTaskMachine {
    id: MachineId,
}

impl VerifiedTaskMachine {
    /// Creates a fresh instance, not yet seeded with a task.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: MachineId::new(),
        }
    }

    /// Permission policy: `RunVerifier` only where verification actually
    /// happens, plus the ordinary read/write/shell/knowledge set an attempt
    /// needs to do real work - identical in spirit to `SdlcMachine::Execution`.
    #[must_use]
    pub fn permission_policy() -> PermissionPolicy {
        use ToolCapability::{
            AssimilateKnowledge, ExecuteShell, GitOperation, IngestDocument, ReadFile, RunVerifier,
            WriteFile,
        };
        PermissionPolicy::builder()
            .allow_globally([ReadFile, AssimilateKnowledge, IngestDocument])
            .allow_in(
                VerifiedTaskState::Attempting,
                [WriteFile, ExecuteShell, GitOperation],
            )
            .allow_in(VerifiedTaskState::Verifying, [RunVerifier])
            .build()
    }
}

impl Default for VerifiedTaskMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds one entry for the [`ATTEMPTS_FACT`] array.
fn attempt_record(index: u32, claimed_done: bool, verdict: Option<&VerifierVerdict>) -> Value {
    let (passed, outcome) = match verdict {
        Some(VerifierVerdict::Passed) => (Some(true), "passed"),
        Some(VerifierVerdict::Failed { .. }) => (Some(false), "failed"),
        Some(VerifierVerdict::NeedsHuman { .. }) => (None, "needs_human"),
        Some(VerifierVerdict::Unknown { .. }) => (None, "unknown"),
        None => (None, "no_claim"),
    };
    json!({
        "index": index,
        "claimed_done": claimed_done,
        "outcome": outcome,
        "passed": passed,
        // A simplified per-attempt reward: 1.0 on a passing verdict, 0.0 for
        // every other outcome. Not yet graded by tool-error ratio the way
        // `sven_session_model::OutcomeFold` grades the whole run - see the
        // module doc's deferred-work note.
        "reward": if passed == Some(true) { 1.0 } else { 0.0 },
    })
}

fn append_attempt(ctx: &mut Context, record: Value) {
    let mut attempts = ctx
        .fact(ATTEMPTS_FACT)
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default();
    attempts.push(record);
    ctx.set_fact(ATTEMPTS_FACT, Value::Array(attempts));
}

/// Builds the turn instruction for one attempt, folding in the previous
/// attempt's failure reason (if any) so a retry has something to act on.
fn attempt_request(
    prompt: &str,
    attempt_index: u32,
    max_attempts: u32,
    last_failure: Option<&str>,
) -> Effect {
    let retry_note = match last_failure {
        Some(reason) if attempt_index > 0 => format!(
            "\n\nThis is attempt {} of {max_attempts}. The previous attempt was \
             checked and did not pass: {reason}\nAddress that before claiming \
             completion again.",
            attempt_index + 1
        ),
        _ => String::new(),
    };
    let instruction = format!(
        "You are attempting a task that will be independently verified after \
         you finish - your own claim of completion is not the check. Use the \
         available tools to do the work for real, then stop calling tools \
         once you believe it is complete; do not describe a pass/fail \
         decision yourself, an external verifier decides that.\n\n\
         Task:\n\"{prompt}\"{retry_note}"
    );
    let tools: Vec<String> = super::sdlc::prompts::WRITE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .collect();
    build_turn_effect(
        THREAD,
        &tools,
        "",
        None,
        Some(&instruction),
        None,
        MAX_ROUNDS_PER_ATTEMPT,
        None,
        None,
    )
}

impl Machine for VerifiedTaskMachine {
    type State = VerifiedTaskState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> VerifiedTaskState {
        VerifiedTaskState::Top
    }

    fn initial(&self) -> VerifiedTaskState {
        VerifiedTaskState::Freeze
    }

    fn superstate(&self, _state: VerifiedTaskState) -> VerifiedTaskState {
        VerifiedTaskState::Top
    }

    fn is_terminal(&self, state: VerifiedTaskState) -> bool {
        matches!(state, VerifiedTaskState::Done | VerifiedTaskState::Parked)
    }

    fn all_states(&self) -> Vec<VerifiedTaskState> {
        use VerifiedTaskState::*;
        vec![Freeze, Attempting, Verifying, Retrying, Done, Parked]
    }

    fn dispatch_state(
        &mut self,
        state: VerifiedTaskState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<VerifiedTaskState> {
        use VerifiedTaskState::*;

        match state {
            Top => Reaction::Ignored,

            // ── Freeze: parse the seed exactly once, pin the verifier ──────
            Freeze => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                Event::UserMessage { text } => {
                    let seed: VerifiedTaskSeed = match serde_json::from_str(text) {
                        Ok(s) => s,
                        Err(e) => {
                            ctx.set_fact(ERROR_FACT, format!("malformed task seed: {e}"));
                            return Reaction::transition(Done, [], "task seed could not be parsed");
                        }
                    };
                    let frozen = FrozenVerifier::freeze(
                        seed.task.verifier.clone(),
                        sven_vocab::verify::VerifierOrigin::Authored {
                            digest: seed.source_digest,
                        },
                    );
                    ctx.set_fact(FROZEN_VERIFIER_FACT, serde_json::to_value(&frozen).unwrap());
                    ctx.set_fact(TASK_ID_FACT, seed.task.id.clone());
                    ctx.set_fact(TASK_PROMPT_FACT, seed.task.prompt.clone());
                    ctx.set_fact(MAX_ATTEMPTS_FACT, seed.task.max_attempts);
                    ctx.set_fact(ATTEMPT_INDEX_FACT, 0u32);
                    ctx.set_fact(ATTEMPTS_FACT, Value::Array(Vec::new()));
                    Reaction::transition(Attempting, [], format!("task {} frozen", seed.task.id))
                }
                _ => Reaction::Ignored,
            },

            // ── Attempting: the ordinary tool loop; a final turn is only a
            //    claim, never parsed as a decision ─────────────────────────
            Attempting => match event {
                Event::Internal(InternalEvent::Entry) => {
                    init_loop(
                        ctx,
                        THREAD,
                        &super::sdlc::prompts::WRITE_TOOLS
                            .iter()
                            .map(|s| s.to_string())
                            .collect::<Vec<_>>(),
                        "",
                        MAX_ROUNDS_PER_ATTEMPT,
                    );
                    let prompt = ctx
                        .fact(TASK_PROMPT_FACT)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let attempt_index = ctx
                        .fact(ATTEMPT_INDEX_FACT)
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    let max_attempts = ctx
                        .fact(MAX_ATTEMPTS_FACT)
                        .and_then(Value::as_u64)
                        .unwrap_or(1) as u32;
                    let last_failure = ctx
                        .fact(ATTEMPTS_FACT)
                        .and_then(Value::as_array)
                        .and_then(|a| a.last())
                        .and_then(|a| a.get("outcome"))
                        .and_then(Value::as_str)
                        .map(|s| s.to_string());
                    let req = attempt_request(
                        &prompt,
                        attempt_index,
                        max_attempts,
                        last_failure.as_deref(),
                    );
                    Reaction::effects(vec![req])
                }
                Event::LlmTurnComplete { .. } => match on_llm_turn_complete(ctx, event) {
                    GeneratingAction::FinalAnswer { .. } => {
                        let frozen: FrozenVerifier = ctx
                            .fact(FROZEN_VERIFIER_FACT)
                            .cloned()
                            .and_then(|v| serde_json::from_value(v).ok())
                            .expect("Freeze always sets a frozen verifier before Attempting runs");
                        Reaction::transition(
                            Verifying,
                            vec![Effect::Verify { spec: frozen.spec }],
                            "attempt claims completion; verifying",
                        )
                    }
                    GeneratingAction::CallTools {
                        tool_effects,
                        calls,
                        ..
                    } => {
                        mark_calls_pending(ctx, &calls);
                        Reaction::effects(tool_effects)
                    }
                    GeneratingAction::EmptyTurn { nudge_effect } => {
                        Reaction::effects(vec![nudge_effect])
                    }
                    GeneratingAction::MaxRoundsReached { .. } => {
                        let attempt_index = ctx
                            .fact(ATTEMPT_INDEX_FACT)
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32;
                        append_attempt(ctx, attempt_record(attempt_index, false, None));
                        next_after_attempt(ctx)
                    }
                },
                Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                    ctx.set_fact(
                        ERROR_FACT,
                        format!("attempt failed to reach the model: {error}"),
                    );
                    Reaction::transition(Done, [], "attempt could not reach the model")
                }
                _ => match handle_tool_event(ctx, |ls| ls.continuation_turn(), event) {
                    Some(reaction) => reaction,
                    None => Reaction::Super(Top),
                },
            },

            // ── Verifying: the only place a verdict can be trusted ─────────
            Verifying => match event {
                Event::Internal(InternalEvent::Entry) => Reaction::handled(),
                // No verdict is not a failed verdict: the task ends ungraded.
                Event::EffectFailed { error, .. } => {
                    ctx.set_fact(ERROR_FACT, format!("the verifier could not run: {error}"));
                    Reaction::transition(Done, [], "verifier could not run")
                }
                Event::VerificationComplete { verdict } => {
                    let frozen: FrozenVerifier = ctx
                        .fact(FROZEN_VERIFIER_FACT)
                        .cloned()
                        .and_then(|v| serde_json::from_value(v).ok())
                        .expect("Freeze always sets a frozen verifier before Verifying runs");
                    if !frozen.still_matches() {
                        // Defense in depth: nothing in this machine can
                        // actually cause this today (see the module doc) -
                        // but if it ever does, the verdict must not be
                        // trusted, so this returns Unknown, not a pass.
                        ctx.set_fact(
                            ERROR_FACT,
                            "frozen verifier no longer matches its pinned hash",
                        );
                        return Reaction::transition(
                            Done,
                            [],
                            "verifier tampered; refusing to grade",
                        );
                    }

                    let attempt_index = ctx
                        .fact(ATTEMPT_INDEX_FACT)
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32;
                    append_attempt(ctx, attempt_record(attempt_index, true, Some(verdict)));

                    match verdict {
                        VerifierVerdict::Passed => {
                            ctx.set_fact(VERDICT_FACT, json!({"passed": true}));
                            Reaction::transition(Done, [], "verifier confirmed completion")
                        }
                        VerifierVerdict::Failed { .. } => next_after_attempt(ctx),
                        VerifierVerdict::NeedsHuman { question, options } => {
                            ctx.set_fact(
                                NEEDS_HUMAN_FACT,
                                json!({"question": question, "options": options}),
                            );
                            Reaction::transition(Parked, [], "verifier needs a human to decide")
                        }
                        VerifierVerdict::Unknown { reason } => {
                            ctx.set_fact(
                                NEEDS_HUMAN_FACT,
                                json!({"question": reason, "options": []}),
                            );
                            Reaction::transition(Parked, [], "verifier could not reach a verdict")
                        }
                    }
                }
                _ => Reaction::Ignored,
            },

            // ── Retrying: a real, audited bounce back into Attempting ──────
            Retrying => match event {
                Event::Internal(InternalEvent::Entry) => {
                    let next_index = ctx
                        .fact(ATTEMPT_INDEX_FACT)
                        .and_then(Value::as_u64)
                        .unwrap_or(0) as u32
                        + 1;
                    ctx.set_fact(ATTEMPT_INDEX_FACT, next_index);
                    Reaction::effects(vec![Effect::EmitInternal {
                        name: RETRY_READY_SIGNAL.to_string(),
                        payload: Value::Null,
                    }])
                }
                Event::Internal(InternalEvent::Custom { name, .. })
                    if name == RETRY_READY_SIGNAL =>
                {
                    Reaction::transition(Attempting, [], "starting next attempt")
                }
                _ => Reaction::Ignored,
            },

            Done | Parked => Reaction::Ignored,
        }
    }
}

/// Decide the next state after an attempt that did not pass: retry while
/// budget remains, otherwise a final, verified failure.
fn next_after_attempt(ctx: &mut Context) -> Reaction<VerifiedTaskState> {
    let attempt_index = ctx
        .fact(ATTEMPT_INDEX_FACT)
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let max_attempts = ctx
        .fact(MAX_ATTEMPTS_FACT)
        .and_then(Value::as_u64)
        .unwrap_or(1) as u32;
    if attempt_index + 1 < max_attempts {
        Reaction::transition(
            VerifiedTaskState::Retrying,
            [],
            "attempt did not pass; retrying",
        )
    } else {
        ctx.set_fact(VERDICT_FACT, json!({"passed": false}));
        Reaction::transition(
            VerifiedTaskState::Done,
            [],
            "retries exhausted without a passing verdict",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::loop_core::on_tool_result;
    use sven_hsm::ids::ToolCallId;
    use sven_hsm::ProposedToolCall;
    use sven_vocab::provenance::ContentDigest;
    use sven_vocab::verify::{Task, VerifierSpec};

    fn seed(id: &str, prompt: &str, verifier: VerifierSpec, max_attempts: u32) -> String {
        serde_json::to_string(&VerifiedTaskSeed {
            task: Task {
                id: id.into(),
                prompt: prompt.into(),
                verifier,
                max_attempts,
            },
            source_digest: ContentDigest::from_hex("deadbeef"),
        })
        .unwrap()
    }

    fn file_exists_spec(path: &str) -> VerifierSpec {
        VerifierSpec::FileExists {
            path: path.into(),
            min_bytes: None,
        }
    }

    /// Drives one event through the machine, following the resulting
    /// transition's `Entry` handler - and, since `Entry` handlers may never
    /// transition themselves (`Retrying` bounces to `Attempting` via a
    /// self-addressed `EmitInternal`/`Custom` round trip instead), simulates
    /// that one round trip too. A real `Runtime` does this via
    /// `InternalExecutor`; this direct `dispatch_state` unit test has no
    /// executor, so it plays that one part by hand.
    fn drive(
        m: &mut VerifiedTaskMachine,
        ctx: &mut Context,
        state: &mut VerifiedTaskState,
        event: Event,
    ) {
        let reaction = m.dispatch_state(*state, &event, ctx);
        if let Reaction::Transition { target, .. } = reaction {
            *state = target;
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
    }

    fn final_turn(text: &str) -> Event {
        Event::LlmTurnComplete {
            thread: THREAD.into(),
            text: text.into(),
            tool_calls: vec![],
        }
    }

    #[test]
    fn freeze_parses_the_seed_and_pins_the_verifier_hash() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);

        let s = seed("t1", "write out.txt", file_exists_spec("out.txt"), 2);
        drive(&mut m, &mut ctx, &mut state, Event::UserMessage { text: s });

        assert_eq!(state, VerifiedTaskState::Attempting);
        let frozen: FrozenVerifier =
            serde_json::from_value(ctx.fact(FROZEN_VERIFIER_FACT).cloned().unwrap()).unwrap();
        assert_eq!(frozen.spec, file_exists_spec("out.txt"));
        assert!(frozen.still_matches());
        assert_eq!(ctx.fact(TASK_ID_FACT).unwrap(), "t1");
    }

    #[test]
    fn a_malformed_seed_goes_to_done_with_no_verdict_ever_set() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);

        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: "not json".into(),
            },
        );

        assert_eq!(state, VerifiedTaskState::Done);
        assert!(
            ctx.fact(VERDICT_FACT).is_none(),
            "a setup error must never read as a graded outcome"
        );
        assert!(ctx.fact(ERROR_FACT).is_some());
    }

    /// A verifier that could not run gives no verdict: the task ends with
    /// an error and is never graded as passed or failed.
    #[test]
    fn a_verifier_that_cannot_run_ends_without_a_verdict() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "write out.txt", file_exists_spec("out.txt"), 1),
            },
        );
        drive(&mut m, &mut ctx, &mut state, final_turn("Done!"));
        assert_eq!(state, VerifiedTaskState::Verifying);

        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::EffectFailed {
                kind: sven_hsm::EffectKind::Verify,
                error: "no verify executor is configured".into(),
            },
        );

        assert_eq!(state, VerifiedTaskState::Done);
        assert!(ctx.fact(VERDICT_FACT).is_none());
        assert!(ctx.fact(ERROR_FACT).is_some());
    }

    /// The headline negative test from the plan: the model claims success,
    /// the file it was supposed to create does not exist, and the outcome
    /// must never be a pass.
    #[test]
    fn a_confident_claim_with_no_evidence_never_passes() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "write out.txt", file_exists_spec("out.txt"), 1),
            },
        );

        drive(&mut m, &mut ctx, &mut state, final_turn("Done!"));
        assert_eq!(state, VerifiedTaskState::Verifying);

        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::VerificationComplete {
                verdict: VerifierVerdict::Failed {
                    reason: "out.txt does not exist".into(),
                },
            },
        );

        assert_eq!(state, VerifiedTaskState::Done);
        let verdict = ctx.fact(VERDICT_FACT).unwrap();
        assert_eq!(
            verdict["passed"], false,
            "a claim with no evidence must never pass"
        );
    }

    #[test]
    fn attempt_one_fails_attempt_two_passes_two_attempts_are_recorded() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "write out.txt", file_exists_spec("out.txt"), 2),
            },
        );

        // Attempt 1: claims done, verifier disagrees.
        drive(&mut m, &mut ctx, &mut state, final_turn("Done!"));
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::VerificationComplete {
                verdict: VerifierVerdict::Failed {
                    reason: "missing".into(),
                },
            },
        );
        assert_eq!(
            state,
            VerifiedTaskState::Attempting,
            "must bounce through Retrying back into Attempting"
        );
        assert_eq!(ctx.fact(ATTEMPT_INDEX_FACT).unwrap(), 1);

        // Attempt 2: claims done, verifier agrees.
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            final_turn("Done, for real this time."),
        );
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::VerificationComplete {
                verdict: VerifierVerdict::Passed,
            },
        );

        assert_eq!(state, VerifiedTaskState::Done);
        assert_eq!(ctx.fact(VERDICT_FACT).unwrap()["passed"], true);

        let attempts = ctx.fact(ATTEMPTS_FACT).unwrap().as_array().unwrap().clone();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["passed"], false);
        assert_eq!(attempts[0]["reward"], 0.0);
        assert_eq!(attempts[1]["passed"], true);
        assert_eq!(attempts[1]["reward"], 1.0);
    }

    #[test]
    fn needs_human_parks_instead_of_passing_or_failing() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed(
                    "t1",
                    "decide something ambiguous",
                    VerifierSpec::AskHuman {
                        question: "did it work?".into(),
                        options: vec![],
                    },
                    1,
                ),
            },
        );

        drive(&mut m, &mut ctx, &mut state, final_turn("Done!"));
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::VerificationComplete {
                verdict: VerifierVerdict::NeedsHuman {
                    question: "did it work?".into(),
                    options: vec![],
                },
            },
        );

        assert_eq!(state, VerifiedTaskState::Parked);
        assert!(
            ctx.fact(VERDICT_FACT).is_none(),
            "parked must never read as a graded outcome"
        );
        assert!(ctx.fact(NEEDS_HUMAN_FACT).is_some());
    }

    /// Adversarial case: nothing in the tool loop can rewrite the frozen
    /// verifier (tools cannot write arbitrary Context facts), so this proves
    /// the guarantee holds by construction rather than by a runtime check -
    /// the verifier a second, hostile "task" tries to supply is simply never
    /// read again after Freeze.
    #[test]
    fn the_frozen_verifier_is_unreachable_once_attempting_starts() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "do it", file_exists_spec("out.txt"), 1),
            },
        );
        let frozen_before = ctx.fact(FROZEN_VERIFIER_FACT).cloned().unwrap();

        // A second UserMessage (as if something tried to re-seed mid-attempt)
        // is simply not handled by Attempting - Freeze is never revisited.
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "do it", file_exists_spec("/etc/passwd"), 1),
            },
        );

        assert_eq!(
            ctx.fact(FROZEN_VERIFIER_FACT).cloned().unwrap(),
            frozen_before,
            "the frozen verifier must not change after Freeze"
        );
    }

    /// Exhausting the tool-round budget without ever producing a claim is
    /// treated as a failed (no-claim) attempt, not given one more "please
    /// wrap up" turn the way `TaskMachine` would - there is nothing to trust
    /// in that text anyway, so ending the attempt immediately is simpler and
    /// no less correct.
    #[test]
    fn max_rounds_reached_counts_as_a_no_claim_attempt() {
        let mut m = VerifiedTaskMachine::new();
        let mut ctx = Context::new();
        let mut state = m.initial();
        let _ = m.dispatch_state(state, &Event::entry(), &mut ctx);
        drive(
            &mut m,
            &mut ctx,
            &mut state,
            Event::UserMessage {
                text: seed("t1", "do it", file_exists_spec("out.txt"), 1),
            },
        );

        // Drive tool-call turns until the round budget is exceeded.
        for _ in 0..(MAX_ROUNDS_PER_ATTEMPT + 1) {
            let call_id = ToolCallId::new();
            let event = Event::LlmTurnComplete {
                thread: THREAD.into(),
                text: String::new(),
                tool_calls: vec![ProposedToolCall {
                    call_id,
                    name: "read_file".into(),
                    args: serde_json::json!({}),
                    capability: ToolCapability::ReadFile,
                }],
            };
            drive(&mut m, &mut ctx, &mut state, event);
            let _ = on_tool_result(&mut ctx, &call_id);
        }

        assert_eq!(
            state,
            VerifiedTaskState::Done,
            "no attempts remain (max_attempts=1)"
        );
        assert_eq!(ctx.fact(VERDICT_FACT).unwrap()["passed"], false);
        let attempts = ctx.fact(ATTEMPTS_FACT).unwrap().as_array().unwrap().clone();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["claimed_done"], false);
        assert_eq!(attempts[0]["outcome"], "no_claim");
    }
}
