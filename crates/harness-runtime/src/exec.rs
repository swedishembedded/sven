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
use sven_hsm::{capability_for_tool_name, Context, Effect, Event, Reaction};
use sven_vocab::verify::VerifierSpec;

use crate::machine::{harness_thread, primitive_word, store_as_of, then_of, HarnessMachine};
use crate::run::{
    child_machine_id, AgentLoop, ChildRun, HarnessRunState, Inflight, ParallelJoin, RetryFrame,
    TraceRecord, Waiting, OUTCOME_FACT,
};
use crate::state::HarnessState;

/// The helper budgets a plan does not set itself: generous enough that any
/// real JSON transform fits, bounded enough that a runaway helper fails as
/// a typed error in bounded time. A plan's `max_helper_fuel` and
/// `max_helper_pages` override them per run.
#[cfg(feature = "transform")]
const DEFAULT_HELPER_FUEL: u64 = 1_000_000;
#[cfg(feature = "transform")]
const DEFAULT_HELPER_PAGES: u32 = 256;

/// The failure code naming the boundary a helper invocation broke. The
/// message travels with it unchanged — the error type's own report.
#[cfg(feature = "transform")]
fn transform_failure_code(err: &sven_harness_transform::TransformError) -> &'static str {
    use sven_harness_transform::{SchemaStage, TransformError};
    match err {
        TransformError::Decode(_) => "helper_decode",
        TransformError::Abi(_) => "helper_abi",
        TransformError::Fuel => "helper_fuel",
        TransformError::Memory => "helper_memory",
        TransformError::Trap(_) => "helper_trap",
        TransformError::Schema {
            stage: SchemaStage::Input,
            ..
        } => "helper_input_schema",
        TransformError::Schema {
            stage: SchemaStage::Output,
            ..
        } => "helper_output_schema",
        TransformError::Contract(_) => "helper_contract",
    }
}

impl HarnessMachine {
    /// Counts a step start against the step budget and records it. The
    /// retry frame's body re-entries carry which attempt they are: the
    /// frame's spent count, counted from 1. `Some` is the failure when the
    /// budget is spent.
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
            attempt: run.retry.as_ref().map_or(1, RetryFrame::attempt),
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
                    open: tools.is_none(),
                    ..AgentLoop::default()
                });
                let effect = self.agent_turn_effect(run, prompt, tools);
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
            #[cfg(not(feature = "transform"))]
            PlanPrimitive::Transform { .. } => self.fail_now(
                run,
                "unsupported_transform",
                "transform steps need the helper runtime, which this build does not carry",
                ctx,
            ),
            #[cfg(feature = "transform")]
            PlanPrimitive::Transform { .. } => self.run_transform(run, &step, ctx),
            PlanPrimitive::Gate { .. } => self.run_gate(run, &step, ctx),
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
                    run.counts.children += 1;
                    let machine = child_machine_id(
                        &name,
                        &step_id,
                        Some(branch.as_str()),
                        run.counts.children,
                    );
                    let descriptor = serde_json::json!({
                        "harness": name,
                        "entry": branch,
                        "input": run.inputs,
                        "depth": depth,
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
                run.trace.push(TraceRecord::FanOut {
                    step: step_id.clone(),
                    branches: branches.clone(),
                });
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
                run.counts.children += 1;
                let machine =
                    child_machine_id(&self.run.plan().name, &step_id, None, run.counts.children);
                let mut descriptor = serde_json::Map::new();
                if *as_kind == DelegateTarget::Machine {
                    descriptor.insert("mode".into(), Value::String(target.clone()));
                } else {
                    descriptor.insert("harness".into(), Value::String(target.clone()));
                }
                descriptor.insert("input".into(), run.resolve_value(input));
                descriptor.insert("depth".into(), Value::from(depth));
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
                run.trace.push(TraceRecord::ChildStarted {
                    step: step_id.clone(),
                    child: machine.as_uuid().to_string(),
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

    /// A transform step: the plan's one pure computation with teeth. The
    /// helper's identity is checked first (a missing helper or a foreign
    /// ABI version is refused before any of its bytes are decoded), then
    /// the bytes are compiled (memoized by content hash), the input is
    /// resolved and run through a fresh sandbox instance, and the walk
    /// carries the output to the step's successor. A helper failure is a
    /// step failure like any other, mapped onto the code naming the
    /// boundary that broke.
    #[cfg(feature = "transform")]
    fn run_transform(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        use base64::Engine as _;
        use sven_harness_transform::Transform;

        let PlanPrimitive::Transform {
            ref helper,
            ref input,
            ref store_as,
            ref then,
        } = step.primitive
        else {
            return self.fail_now(
                run,
                "internal",
                "transform dispatch reached a non-transform step",
                ctx,
            );
        };
        let Some(spec) = self.run.helper(helper) else {
            return self.fail_now(
                run,
                "helper_missing",
                &format!(
                    "transform step `{}` names helper `{helper}`, which the run does not carry",
                    step.id
                ),
                ctx,
            );
        };
        if spec.abi != sven_harness_transform::HELPER_ABI {
            return self.fail_now(
                run,
                "helper_abi",
                &format!(
                    "helper `{helper}` was written against ABI `{}`, this runtime implements `{}`",
                    spec.abi,
                    sven_harness_transform::HELPER_ABI
                ),
                ctx,
            );
        }
        if spec.language != "wasm" {
            return self.fail_now(
                run,
                "helper_language",
                &format!(
                    "helper `{helper}` is {}, but only `wasm` helpers run here",
                    spec.language
                ),
                ctx,
            );
        }
        let bytes = match base64::engine::general_purpose::STANDARD.decode(&spec.content) {
            Ok(bytes) => bytes,
            Err(_) => {
                return self.fail_now(
                    run,
                    "helper_encoding",
                    &format!("helper `{helper}` is not valid base64"),
                    ctx,
                );
            }
        };
        let digest = sven_harness_transform::digest(&bytes);
        // A cache miss only decodes and validates: decode failure is the
        // one way this fails, reported by the error type's own Display.
        let module = match self.helper_modules.load_hashed(digest, &bytes) {
            Ok(module) => module,
            Err(err) => return self.fail_now(run, "helper_decode", &err.to_string(), ctx),
        };
        let transform = match Transform::from_module(module, digest, None, None) {
            Ok(transform) => transform,
            Err(err) => {
                return self.fail_now(run, "helper_abi", &err.to_string(), ctx);
            }
        };
        let limits = sven_harness_transform::Limits {
            fuel: self
                .run
                .plan()
                .limits
                .as_ref()
                .and_then(|l| l.max_helper_fuel)
                .unwrap_or(DEFAULT_HELPER_FUEL),
            max_pages: self
                .run
                .plan()
                .limits
                .as_ref()
                .and_then(|l| l.max_helper_pages)
                .unwrap_or(DEFAULT_HELPER_PAGES),
        };
        let value = run.resolve_value(input);
        match transform.run(&value, &limits) {
            Ok(out) => {
                run.trace.push(TraceRecord::StepCompleted {
                    step: step.id.clone(),
                    call_id: None,
                });
                run.store_step_value(&step.id, out, store_as.as_deref());
                self.walk_to(run, then, ctx)
            }
            Err(err) => self.step_failed(run, transform_failure_code(&err), &err.to_string(), ctx),
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

    /// A step whose result is already stored is done: record it, end the
    /// retry frame it was the body of, and point the walk at `next`.
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
        // The body succeeded: the retry is over, and the steps after it are
        // not attempts of it (nor does a later failure re-run the body).
        if run
            .retry
            .as_ref()
            .is_some_and(|frame| frame.body == step_id)
        {
            run.retry = None;
        }
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
        run.retry = None;
        run.agent = None;
        run.child = None;
        run.join = None;
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
        run.join = None;
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
