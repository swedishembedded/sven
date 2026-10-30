// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Primitive execution: the walk.
//!
//! [`execute_current`] is the one place a plan primitive is interpreted.
//! Pure primitives (`assign`, `branch`, and their walking) resolve inline and
//! keep re-entering the walk until it parks on a side effect or reaches a
//! terminal step; side-effecting primitives emit their typed effect and park.
//! Step completions feed back into the walk, so one dispatch runs a whole pure
//! chain and everything that happens between two pieces of I/O.

use serde_json::Value;
use sven_harness_spec::{PlanPrimitive, PlanStep};
use sven_hsm::{Context, Effect, Event, Reaction};
use sven_vocab::verify::VerifierSpec;

use crate::decoding::{resolve_decoding, TurnShape};

use crate::decoding::conversation_thread;
use crate::machine::{primitive_word, store_as_of, then_of, HarnessMachine};
use crate::run::{AgentLoop, HarnessRunState, TraceRecord, Waiting, OUTCOME_FACT};
use crate::state::HarnessState;

impl HarnessMachine {
    /// Counts a step start against the step budget and records it. `Some` is
    /// the failure when the budget is spent.
    pub(super) fn begin_step(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Option<Reaction<HarnessState>> {
        run.counts.steps += 1;
        if run.counts.steps > run.step_budget(self.run.plan()) {
            return Some(self.fail_now(
                run,
                "step_budget",
                "the run exceeded its step budget",
                ctx,
            ));
        }
        run.trace.push(TraceRecord::StepStarted {
            step: step.id.clone(),
            primitive: primitive_word(&step.primitive).to_string(),
        });
        None
    }
}

impl HarnessMachine {
    /// Executes the step the walk points at: the one place a primitive is
    /// interpreted. Pure primitives resolve inline and point the walk at
    /// their successor (as a new advance signal); side-effecting
    /// primitives park the run and emit their effect as a transition
    /// action.
    pub(super) fn execute_current(
        &mut self,
        run: &mut HarnessRunState,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let step_id = run.current.clone();
        let Some(step) = self.run.plan().step(&step_id).cloned() else {
            return self.fail_now(
                run,
                "unknown_step",
                &format!("step `{step_id}` does not exist"),
                ctx,
            );
        };
        if let Some(spent) = self.begin_step(run, &step, ctx) {
            return spent;
        }
        match step.primitive.clone() {
            PlanPrimitive::Assign { set, then } => {
                for (name, raw) in set {
                    let value = run.resolve_value(&raw);
                    run.facts.insert(name, value);
                }
                self.walk_to(run, &then, ctx)
            }
            PlanPrimitive::Branch {
                on,
                when_true,
                when_false,
            } => {
                let condition = run.eval_expr(&on);
                let chosen = if condition { when_true } else { when_false };
                run.trace.push(TraceRecord::BranchSelected {
                    step: step_id.clone(),
                    condition,
                    selected: chosen.clone(),
                });
                self.walk_to(run, &chosen, ctx)
            }
            PlanPrimitive::Predict {
                ref prompt,
                ref schema,
                ref conversation,
                ref system,
                ref origin,
                ref role,
                ref decoding,
                ..
            } => {
                let decoding = match resolve_decoding(run, decoding) {
                    Ok(d) => d,
                    Err(why) => return self.fail_now(run, "invalid_setting", &why, ctx),
                };
                run.answer_repairs = 0;
                run.counts.model_turns += 1;
                if run.exceeds_model_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "model_turn_budget",
                        "the run exceeded its model-turn budget",
                        ctx,
                    );
                }
                let thread = conversation_thread(self.run.plan(), run, conversation.as_ref());
                let effect = self.turn_effect(
                    run,
                    prompt,
                    schema.as_deref(),
                    TurnShape {
                        thread: &thread,
                        system: system.as_ref(),
                        origin: origin.as_deref(),
                        role: role.as_deref(),
                        decoding,
                    },
                );
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Model);
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForModel,
                    effects: vec![effect],
                    rationale: format!("predict step `{step_id}` asks the model"),
                }
            }
            PlanPrimitive::Agent {
                ref prompt,
                ref tools,
                ref conversation,
                ref vars,
                ref system,
                ref origin,
                ref role,
                ref decoding,
                ref stop_as,
                ref pool,
                ref allowance,
                max_repeated_results,
                ref attach,
                ..
            } => {
                let decoding = match resolve_decoding(run, decoding) {
                    Ok(d) => d,
                    Err(why) => return self.fail_now(run, "invalid_setting", &why, ctx),
                };
                if let (Some(pool), Some(allowance)) = (pool, allowance) {
                    // A literal written in the document reaches here as text.
                    let calls = match run.resolve_template(allowance) {
                        serde_json::Value::String(text) => text.trim().parse::<u64>().ok(),
                        other => other.as_u64(),
                    };
                    match calls {
                        Some(calls) => {
                            run.facts
                                .insert(pool.clone(), serde_json::Value::from(calls));
                        }
                        None => {
                            return self.fail_now(
                                run,
                                "invalid_setting",
                                "the tool-call allowance is not a whole number",
                                ctx,
                            );
                        }
                    }
                }
                run.counts.model_turns += 1;
                if run.exceeds_model_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "model_turn_budget",
                        "the run exceeded its model-turn budget",
                        ctx,
                    );
                }
                let thread = conversation_thread(self.run.plan(), run, conversation.as_ref());
                run.agent = Some(AgentLoop {
                    grants: vars.clone(),
                    stop_as: stop_as.clone(),
                    role: role.clone(),
                    pool: pool.clone(),
                    max_repeats: max_repeated_results,
                    step: step_id.clone(),
                    thread: thread.clone(),
                    tools: tools.clone().unwrap_or_default(),
                    open: tools.is_none(),
                    decoding,
                    ..AgentLoop::default()
                });
                let attachments = attach
                    .iter()
                    .map(|path| match run.resolve_template(path) {
                        serde_json::Value::String(text) => text,
                        other => other.to_string(),
                    })
                    .collect();
                let effect = self.agent_turn_effect(
                    run,
                    prompt,
                    tools,
                    attachments,
                    TurnShape {
                        thread: &thread,
                        system: system.as_ref(),
                        origin: origin.as_deref(),
                        role: role.as_deref(),
                        decoding,
                    },
                );
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Model);
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForModel,
                    effects: vec![effect],
                    rationale: format!("agent step `{step_id}` opens its loop"),
                }
            }
            PlanPrimitive::Verify { ref spec, .. } => {
                // A file check may name the loop's item.
                let resolved = run.resolve_value(spec);
                let parsed: VerifierSpec =
                    serde_json::from_value(resolved).unwrap_or(VerifierSpec::Unsupported);
                let effect = Effect::Verify { spec: parsed };
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Verifier);
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForVerifier,
                    effects: vec![effect],
                    rationale: format!("verify step `{step_id}` grades the run"),
                }
            }
            PlanPrimitive::Return { ref value } => {
                let outcome = run.resolve_value(value);
                run.trace.push(TraceRecord::Outcome {
                    outcome: outcome.clone(),
                });
                run.store(ctx);
                ctx.set_fact(OUTCOME_FACT, outcome.clone());
                self.outcome_cache = Some(outcome);
                Reaction::Transition {
                    target: HarnessState::Completed,
                    effects: vec![Effect::PersistAudit],
                    rationale: format!("return step `{step_id}` completed the run"),
                }
            }
            PlanPrimitive::Fail {
                ref code,
                ref message,
            } => {
                let code = code.clone().unwrap_or_else(|| "harness_failed".to_string());
                let message = message
                    .as_ref()
                    .map(|m| run.resolve_template(m))
                    .unwrap_or_default();
                let text = message.as_str().unwrap_or_default().to_string();
                self.fail_now(run, &code, &text, ctx)
            }
            PlanPrimitive::Gate { .. } => self.run_gate(run, &step, ctx),
            PlanPrimitive::Iterate { .. } => self.run_iterate(run, &step, ctx),
            PlanPrimitive::Delegate { .. } => self.run_delegate(run, &step, ctx),
        }
    }

    // ── completions, failures, walk helpers ─────────────────────────────

    /// A side effect completed: store the result, follow the step's `then`
    /// edge, and keep walking — pure steps run inline right here, so the
    /// run rests either on the next side effect's park (a transition to a
    /// waiting state carrying its effect), or terminal, or idle-on-a-pure
    /// step (handled, no effects; the next tick advances). Either way the
    /// completion dispatch itself emits nothing.
    pub(super) fn complete_step(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        value: Value,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let store_as = self
            .run
            .plan()
            .step(step_id)
            .and_then(|step| store_as_of(&step.primitive));
        run.store_step_value(step_id, value, store_as.as_deref());
        let then = self
            .run
            .plan()
            .step(step_id)
            .map(|step| then_of(&step.primitive))
            .unwrap_or_default();
        self.settle_step(run, step_id, &then, ctx)
    }

    /// A step whose result is already stored is done: record it and point the
    /// walk at `next`.
    pub(super) fn settle_step(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        next: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.trace.push(TraceRecord::StepCompleted {
            step: step_id.to_string(),
            call_id: run.inflight.as_ref().and_then(|inflight| inflight.call_id),
        });
        run.awaiting = None;
        run.inflight = None;
        run.current = next.to_string();
        run.store(ctx);
        // The walk advances through Advancing: its entry emits the advance
        // signal, which the kernel's executor bounces back and which then
        // executes the step the walk points at. A run never rests after a
        // stored completion waiting for an external tick - a spawned child
        // has no tick driver.
        Reaction::Transition {
            target: HarnessState::Advancing,
            effects: Vec::new(),
            rationale: format!("step `{step_id}` completed"),
        }
    }

    /// A side effect failed: the run fails.
    pub(super) fn side_effect_failed(
        &mut self,
        run: &mut HarnessRunState,
        error: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        self.fail_now(run, "side_effect_failed", error, ctx)
    }

    /// The walk moves to `step` and keeps going: pure steps run inline (one
    /// dispatch executes a whole pure chain), a side-effecting step parks
    /// the run (the returned reaction transitions to its waiting state
    /// carrying the effect). This is the loop that makes a chain advance
    /// without an external tick.
    pub(super) fn walk_to(
        &mut self,
        run: &mut HarnessRunState,
        step: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        run.current = step.to_string();
        self.execute_current(run, ctx)
    }

    /// A tick (a message) in a state whose side effect already completed:
    /// execute the step the walk points at.
    pub(super) fn tick_when_idle(
        &mut self,
        run: &mut HarnessRunState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        if !matches!(event, Event::UserMessage { .. }) {
            return Reaction::Ignored;
        }
        self.execute_current(run, ctx)
    }

    /// Fails the run with a code and (already resolved) message.
    pub(super) fn fail_now(
        &mut self,
        run: &mut HarnessRunState,
        code: &str,
        message: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        // The in-flight operation's id: what this failure belonged to (a
        // tool call, or the child run whose result carried it).
        let operation = run
            .inflight
            .as_ref()
            .and_then(|inflight| inflight.call_id)
            .map(|call_id| call_id.as_uuid().to_string())
            .or_else(|| run.child.as_ref().map(|child| child.machine.clone()));
        run.trace.push(TraceRecord::Failed {
            step: run.current.clone(),
            code: code.to_string(),
            message: message.to_string(),
            operation,
        });
        run.awaiting = None;
        run.inflight = None;
        run.agent = None;
        run.child = None;
        run.store(ctx);
        let outcome = serde_json::json!({
            "code": code,
            "message": message,
        });
        ctx.set_fact(OUTCOME_FACT, outcome.clone());
        self.outcome_cache = Some(outcome);
        Reaction::Transition {
            target: HarnessState::Failed,
            effects: vec![Effect::PersistAudit],
            rationale: format!("run failed: {code}"),
        }
    }

    /// Cancels the run from anywhere.
    pub(super) fn cancel(&mut self, ctx: &mut Context) -> Reaction<HarnessState> {
        let mut run = self.run_state(ctx);
        let outcome = serde_json::json!({
            "code": "cancelled",
            "message": "the user cancelled the run",
        });
        run.trace.push(TraceRecord::Outcome {
            outcome: outcome.clone(),
        });
        run.awaiting = None;
        run.inflight = None;
        run.child = None;
        run.store(ctx);
        ctx.set_fact(OUTCOME_FACT, outcome.clone());
        self.outcome_cache = Some(outcome);
        Reaction::Transition {
            target: HarnessState::Cancelled,
            effects: vec![Effect::PersistAudit],
            rationale: "run cancelled".into(),
        }
    }

    /// Like [`Self::cache_effect`] but for `Option<Effect>`.
    pub(super) fn cache_effect_opt(&mut self, effect: Option<Effect>) {
        if let Some(effect) = effect {
            self.cache_effect(effect);
        }
    }
}
