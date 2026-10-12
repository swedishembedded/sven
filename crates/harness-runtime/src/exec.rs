// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Primitive execution: the walk.
//!
//! [`execute_current`] is the one place a plan primitive is interpreted.
//! Pure primitives (`assign`, `branch`, `retry`, and their walking) resolve
//! inline and keep re-entering the walk until it parks on a side effect or
//! reaches a terminal step; side-effecting primitives emit their typed
//! effect and park. Step completions and retry loop-backs feed back into
//! the walk, so one dispatch runs a whole pure chain and everything that
//! happens between two pieces of I/O.

use serde_json::Value;
use sven_harness_spec::{DelegateTarget, PlanPrimitive, PlanStep};
use sven_hsm::{capability_for_tool_name, Context, Effect, Event, MachineId, Reaction};
use sven_vocab::verify::VerifierSpec;

use crate::machine::{harness_thread, store_as_of, then_of, HarnessMachine};
use crate::run::{
    AgentLoop, ChildRun, HarnessRunState, Inflight, ParallelJoin, RetryFrame, Waiting,
};
use crate::state::HarnessState;

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
        run.counts.steps += 1;
        if run.counts.steps > run.step_budget(self.run.plan()) {
            return self.fail_now(run, "step_budget", "the run exceeded its step budget", ctx);
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
                let chosen = if run.eval_expr(&on) {
                    when_true
                } else {
                    when_false
                };
                self.walk_to(run, &chosen, ctx)
            }
            PlanPrimitive::Retry { attempts, body, .. } => {
                run.retry = Some(RetryFrame {
                    step: step_id,
                    body: body.clone(),
                    attempts,
                    left: attempts,
                });
                self.walk_to(run, &body, ctx)
            }
            PlanPrimitive::Predict {
                ref prompt,
                ref schema,
                ..
            } => {
                run.counts.model_turns += 1;
                if run.exceeds_model_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "model_turn_budget",
                        "the run exceeded its model-turn budget",
                        ctx,
                    );
                }
                let effect = self.turn_effect(run, prompt, schema.as_deref());
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
                ..
            } => {
                run.counts.model_turns += 1;
                if run.exceeds_model_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "model_turn_budget",
                        "the run exceeded its model-turn budget",
                        ctx,
                    );
                }
                run.agent = Some(AgentLoop {
                    step: step_id.clone(),
                    thread: harness_thread(self.run.plan()),
                    tools: tools.clone().unwrap_or_default(),
                    pending: Vec::new(),
                    rounds: 0,
                });
                let effect = self.turn_effect(run, prompt, None);
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Model);
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForModel,
                    effects: vec![effect],
                    rationale: format!("agent step `{step_id}` opens its loop"),
                }
            }
            PlanPrimitive::Tool {
                ref name, ref args, ..
            } => {
                run.counts.tool_calls += 1;
                if run.exceeds_tool_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "tool_call_budget",
                        "the run exceeded its tool-call budget",
                        ctx,
                    );
                }
                let call_id = self.call_id_for(&step, run);
                let effect = Effect::CallTool {
                    call_id,
                    name: name.clone(),
                    capability: capability_for_tool_name(name),
                    args: run.resolve_value(args),
                };
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Tool);
                run.inflight = Some(Inflight {
                    step: step_id.clone(),
                    call_id: Some(call_id),
                });
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForTool,
                    effects: vec![effect],
                    rationale: format!("tool step `{step_id}` calls `{name}`"),
                }
            }
            PlanPrimitive::Verify { ref spec, .. } => {
                let parsed: VerifierSpec =
                    serde_json::from_value(spec.clone()).unwrap_or(VerifierSpec::Unsupported);
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
                run.store(ctx);
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
            PlanPrimitive::Transform { .. } => self.fail_now(
                run,
                "unsupported_transform",
                "transform steps need the helper runtime, which this build does not carry",
                ctx,
            ),
            PlanPrimitive::Parallel { ref branches, .. } => {
                // Each branch child runs one level below this run; the
                // depth budget bounds recursive fan-out the same way it
                // bounds delegation.
                let depth = run.delegate_depth + 1;
                let max = self
                    .run
                    .plan()
                    .limits
                    .as_ref()
                    .and_then(|l| l.max_delegate_depth);
                if let Some(max) = max {
                    if depth > max {
                        return self.fail_now(
                            run,
                            "delegate_depth",
                            &format!("fan-out would exceed max_delegate_depth ({max})"),
                            ctx,
                        );
                    }
                }
                // One child per branch, in declared order; the join record
                // is the correlation table for their completions. Branches
                // run this same plan from their step, with the run's input
                // namespace (compile-time max_parallel bounds the count).
                let name = self.run.plan().name.clone();
                let mut effects = Vec::new();
                let mut machines = Vec::new();
                for branch in branches {
                    let machine = MachineId::new();
                    let descriptor = serde_json::json!({
                        "harness": name,
                        "entry": branch,
                        "input": run.inputs,
                    });
                    effects.push(Effect::InstantiateSubmachine {
                        machine,
                        descriptor,
                    });
                    machines.push((branch.clone(), machine.as_uuid().to_string()));
                }
                if let Some(effect) = effects.first() {
                    self.cache_effect(effect.clone());
                }
                self.park(run, &step, Waiting::Join);
                let fan_out = branches.len();
                run.join = Some(ParallelJoin {
                    step: step_id.clone(),
                    results: vec![None; fan_out],
                    machines,
                });
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForJoin,
                    effects,
                    rationale: format!("parallel step `{step_id}` fans out {fan_out} branches"),
                }
            }
            PlanPrimitive::Delegate {
                ref target,
                ref as_kind,
                ref input,
                ..
            } => {
                // Depth: this run sits at its own depth; a child would live
                // one below it. The plan's `max_delegate_depth` counts the
                // top-level run as 1, so a child needs depth < max.
                let depth = run.delegate_depth + 1;
                let max = self
                    .run
                    .plan()
                    .limits
                    .as_ref()
                    .and_then(|l| l.max_delegate_depth);
                if let Some(max) = max {
                    if depth > max {
                        return self.fail_now(
                            run,
                            "delegate_depth",
                            &format!("delegation would exceed max_delegate_depth ({max})"),
                            ctx,
                        );
                    }
                }
                let machine = MachineId::new();
                let mut descriptor = serde_json::Map::new();
                if *as_kind == DelegateTarget::Machine {
                    descriptor.insert("mode".into(), Value::String(target.clone()));
                } else {
                    descriptor.insert("harness".into(), Value::String(target.clone()));
                }
                descriptor.insert("input".into(), run.resolve_value(input));
                let effect = Effect::InstantiateSubmachine {
                    machine,
                    descriptor: Value::Object(descriptor),
                };
                self.cache_effect(effect.clone());
                self.park(run, &step, Waiting::Child);
                run.child = Some(ChildRun {
                    machine: machine.as_uuid().to_string(),
                    step: step_id.clone(),
                });
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForChild,
                    effects: vec![effect],
                    rationale: format!("delegate step `{step_id}` starts child `{target}`"),
                }
            }
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
        run.awaiting = None;
        run.inflight = None;
        let then = self
            .run
            .plan()
            .step(step_id)
            .map(|step| then_of(&step.primitive))
            .unwrap_or_default();
        run.current = then;
        run.store(ctx);
        // Effect-free: the pure tail waits for the next tick (the driver's
        // UserMessage advance), keeping completion dispatches I/O-clean.
        Reaction::handled()
    }

    /// A side effect failed: loop the active retry frame (straight back
    /// into the body's waiting state, the next attempt's call emitted at
    /// once) or fail the run.
    pub(super) fn side_effect_failed(
        &mut self,
        run: &mut HarnessRunState,
        error: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        self.step_failed(run, "side_effect_failed", error, ctx)
    }

    /// A schema'd predict answer did not parse as JSON: the same retry
    /// handling as any other failed step, with its own failure code.
    pub(super) fn invalid_answer(
        &mut self,
        run: &mut HarnessRunState,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        self.step_failed(run, "invalid_answer", "answer is not valid JSON", ctx)
    }

    fn step_failed(
        &mut self,
        run: &mut HarnessRunState,
        code: &str,
        error: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(frame) = run.retry.clone() else {
            return self.fail_now(run, code, error, ctx);
        };
        if frame.left <= 1 {
            run.retry = None;
            run.awaiting = None;
            run.inflight = None;
            run.child = None;
            return self.walk_to(run, &frame.step, ctx);
        }
        let mut frame = frame;
        frame.left -= 1;
        let body = frame.body.clone();
        run.retry = Some(frame);
        run.awaiting = None;
        run.inflight = None;
        run.child = None;
        run.current = body.clone();
        let Some(step) = self.run.plan().step(&body).cloned() else {
            return self.fail_now(
                run,
                "unknown_step",
                &format!("retry body `{body}` does not exist"),
                ctx,
            );
        };
        self.run_body(run, &step, ctx)
    }

    /// Re-runs a retry body's side-effecting primitive: the body is always
    /// a step with a side effect (a frame only exists when one failed).
    fn run_body(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        match &step.primitive {
            PlanPrimitive::Tool {
                ref name, ref args, ..
            } => {
                run.counts.tool_calls += 1;
                if run.exceeds_tool_budget(self.run.plan()) {
                    return self.fail_now(
                        run,
                        "tool_call_budget",
                        "the run exceeded its tool-call budget",
                        ctx,
                    );
                }
                let call_id = self.call_id_for(step, run);
                let effect = Effect::CallTool {
                    call_id,
                    name: name.clone(),
                    capability: capability_for_tool_name(name),
                    args: run.resolve_value(args),
                };
                self.cache_effect(effect.clone());
                self.park(run, step, Waiting::Tool);
                run.inflight = Some(Inflight {
                    step: step.id.clone(),
                    call_id: Some(call_id),
                });
                run.store(ctx);
                Reaction::Transition {
                    target: HarnessState::WaitingForTool,
                    effects: vec![effect],
                    rationale: format!("retry re-entered body step `{}`", step.id),
                }
            }
            // A pure body cannot fail a side effect; treat reaching here as
            // the walk simply moving on.
            _ => self.walk_to(run, &step.id, ctx),
        }
    }

    /// The walk moves to `step` and keeps going: pure steps run inline (one
    /// dispatch executes a whole pure chain), a side-effecting step parks
    /// the run (the returned reaction transitions to its waiting state
    /// carrying the effect). This is the loop that makes a chain advance
    /// without an external tick.
    fn walk_to(
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
        run.awaiting = None;
        run.inflight = None;
        run.retry = None;
        run.agent = None;
        run.child = None;
        run.store(ctx);
        self.outcome_cache = Some(serde_json::json!({
            "code": code,
            "message": message,
        }));
        Reaction::Transition {
            target: HarnessState::Failed,
            effects: vec![Effect::PersistAudit],
            rationale: format!("run failed: {code}"),
        }
    }

    /// Cancels the run from anywhere.
    pub(super) fn cancel(&mut self, ctx: &mut Context) -> Reaction<HarnessState> {
        let mut run = self.run_state(ctx);
        run.awaiting = None;
        run.inflight = None;
        run.child = None;
        run.store(ctx);
        self.outcome_cache = Some(serde_json::json!({
            "code": "cancelled",
            "message": "the user cancelled the run",
        }));
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
