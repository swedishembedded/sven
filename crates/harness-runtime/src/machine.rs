// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`HarnessMachine`]: the one generic machine that interprets compiled
//! harness plans.
//!
//! The machine is a dispatcher, not a parser: at every step it matches the
//! plan step's primitive, does the pure work inline (store, resolve,
//! branch), emits the kernel's typed effects for the impure primitives, and
//! points the walk at the successor step. There is no second engine here —
//! every side effect is an existing kernel `Effect`, every completion an
//! existing kernel `Event`, and the permission gate sees nothing it does
//! not already see for the hardcoded machines.
//!
//! # Driving a run
//!
//! * A `UserMessage` **starts** the run (in [`HarnessState::Ready`]) and is
//!   the universal **tick** afterwards: whenever nothing is in flight, a
//!   message executes the step the walk points at. A pure chain (assign,
//!   branch) advances itself through the machine's
//!   internal advance signal — the kernel's executor bounces each
//!   `EmitInternal` back as an internal event, the same round trip the
//!   other machines use.
//! * An impure primitive parks the run in its `Waiting*` state and emits
//!   its effect as a transition action. The completion event stores the
//!   result, follows the step's `then` edge, and leaves the run resting:
//!   the next tick advances. A failed side effect fails the run.
//! * A terminal state absorbs everything.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use serde_json::Value;
use sven_harness_spec::{HarnessPlan, PlanPrimitive, PlanStep, Template};
use sven_hsm::{
    Context, Effect, Event, InternalEvent, Machine, MachineId, Reaction, ToolCallId,
    ToolCapability, TURN_TRUNCATED, TURN_WATCHDOG,
};
use sven_vocab::verify::VerifierVerdict;
use sven_vocab::TurnRequest;

use crate::command_gate::TurnTrouble;
use crate::decoding::{system_text, TurnShape};
use crate::host_values::plan_reads_scratchpad;
use crate::run::{step_call_id, HarnessRunData, HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::{superstate_of, HarnessState};

/// The machine's internal advance signal: the kernel's executor re-enters
/// it as `InternalEvent::Custom`, and the handler executes the step the
/// walk points at. This is how a pure chain advances without an external
/// tick.
pub(super) const ADVANCE_SIGNAL: &str = "harness.advance";

/// Runs one harness plan through the HSM kernel.
///
/// The struct holds only identity, the compiled program, and the two
/// conveniences a driver reads between dispatches (the emitted step effect
/// and the terminal outcome). Every durable bit of run state lives in the
/// [`Context`] under [`RUN_STATE_FACT`](crate::RUN_STATE_FACT).
pub struct HarnessMachine {
    pub(super) id: MachineId,
    pub(super) run: HarnessRunData,
    /// The leaf this machine's transitions moved the walk to. Snapshot
    /// labels come from here; a restored machine takes its leaf from the
    /// snapshot instead.
    pub(super) leaf: HarnessState,
    /// The effect of the step the walk most recently parked on — what
    /// [`HarnessMachine::step_effect`] answers.
    pub(super) step_effect_cache: Option<Effect>,
    /// The outcome, once a terminal step executed.
    pub(super) outcome_cache: Option<Value>,
}

impl HarnessMachine {
    /// Creates a machine for a validated run.
    #[must_use]
    pub fn new(run: HarnessRunData) -> Self {
        Self {
            id: MachineId::new(),
            leaf: HarnessState::Ready,
            run,
            step_effect_cache: None,
            outcome_cache: None,
        }
    }

    /// The compiled plan this machine executes.
    #[must_use]
    pub fn plan(&self) -> &HarnessPlan {
        self.run.plan()
    }

    /// The run data this machine was built with: the plan, the inputs, and
    /// — for a bundle run — the pinned bundle digest.
    #[must_use]
    pub fn run(&self) -> &HarnessRunData {
        &self.run
    }

    /// The effect the most recently entered step emitted, if the walk is
    /// parked on a side-effecting step. A convenience for drivers that
    /// deliver effects one at a time; the kernel's runtime takes effects
    /// from dispatch outcomes instead.
    #[must_use]
    pub fn step_effect(&self) -> Option<Effect> {
        self.step_effect_cache.clone()
    }

    /// The run's outcome once it rests in a terminal state: a `return`
    /// step's resolved value, or a `fail` step's `{code, message}` object.
    #[must_use]
    pub fn outcome(&self) -> Option<Value> {
        self.outcome_cache.clone()
    }

    /// `true` when the run rests in a terminal state.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.leaf.is_terminal()
    }

    /// The run's structured execution trace, as recorded in the run state
    /// (so it travels with a checkpoint).
    #[must_use]
    pub fn trace(&self, ctx: &Context) -> Vec<TraceRecord> {
        HarnessRunState::load(ctx).trace
    }

    // ── run-state and effect helpers ────────────────────────────────────

    pub(super) fn run_state(&self, ctx: &Context) -> HarnessRunState {
        HarnessRunState::load(ctx)
    }

    /// `true` when `step` is a `gate` step.
    pub(super) fn is_gate_step(&self, step: &str) -> bool {
        matches!(
            self.plan().step(step).map(|s| &s.primitive),
            Some(PlanPrimitive::Gate { .. })
        )
    }

    /// Records the leaf this machine transitioned to.
    pub(super) fn set_leaf(&mut self, leaf: HarnessState) {
        self.leaf = leaf;
    }

    /// The value a completed predict step stores. A step without a schema
    /// stores the raw text; a step that named a schema must return a JSON
    /// document, otherwise the step failed its contract.
    pub(super) fn answer_value(&self, step_id: &str, text: &str) -> Option<Value> {
        let schema = match self.plan().step(step_id) {
            Some(sven_harness_spec::PlanStep {
                primitive: PlanPrimitive::Predict { schema, .. },
                ..
            }) => schema.clone(),
            _ => None,
        };
        if schema.is_none() {
            return Some(Value::String(text.to_string()));
        }
        serde_json::from_str(text)
            .ok()
            .or_else(|| serde_json::from_str(unfenced(text)?).ok())
    }

    /// The deterministic call id of `step`.
    pub(super) fn call_id_for(&self, step: &PlanStep) -> ToolCallId {
        step_call_id(&self.plan().name, &step.id)
    }

    /// Builds a single-turn `CallLlm` request for `prompt`: no tools, no
    /// all-tools fallback, no tool rounds — a constrained turn. When the
    /// step names a declared schema, the turn carries it as the response
    /// format.
    pub(super) fn turn_effect(
        &self,
        state: &HarnessRunState,
        prompt: &Template,
        schema_name: Option<&str>,
        shape: TurnShape<'_>,
    ) -> Effect {
        let instruction = state.resolve_template(prompt);
        let mut schema = Value::Null;
        let mut name = String::new();
        if let Some(want) = schema_name {
            if let Some(body) = self.plan().schemas.as_ref().and_then(|s| s.get(want)) {
                name = want.to_string();
                schema = body.clone();
            }
        }
        let request = TurnRequest {
            thread: shape.thread.to_string(),
            instruction: instruction.as_str().unwrap_or_default().to_string(),
            tools: Vec::new(),
            all_tools_mode: String::new(),
            active_tools: Vec::new(),
            active_instructions: Vec::new(),
            state: String::new(),
            schema,
            schema_name: name,
            model: None,
            dynamic_suffix: None,
            max_tool_rounds: Some(0),
            answered_calls: Vec::new(),
            origin: shape.origin.map(str::to_string),
            role: shape.role.map(str::to_string),
            system: system_text(state, shape.system),
            decoding: shape.decoding,
            delay_ms: (state.retry_delay_ms > 0).then_some(state.retry_delay_ms),
        };
        Effect::CallLlm {
            request: request.to_value(),
        }
    }

    /// Parks the run on `step`: records what is in flight so the
    /// completion events can be matched.
    pub(super) fn park(&mut self, state: &mut HarnessRunState, step: &PlanStep, waiting: Waiting) {
        let call_id = matches!(waiting, Waiting::Tool).then(|| self.call_id_for(step));
        state.awaiting = Some(waiting);
        state.inflight = Some(Inflight {
            step: step.id.clone(),
            call_id,
        });
    }

    /// Remembers the effect a parked step emitted, for `step_effect`.
    pub(super) fn cache_effect(&mut self, effect: Effect) {
        self.step_effect_cache = Some(effect);
    }

    /// The advance signal for the step the walk points at.
    fn advance_signal(step: &str) -> Effect {
        Effect::EmitInternal {
            name: ADVANCE_SIGNAL.to_string(),
            payload: serde_json::json!({ "step": step }),
        }
    }
}

impl Machine for HarnessMachine {
    type State = HarnessState;

    fn id(&self) -> MachineId {
        self.id
    }

    fn top(&self) -> Self::State {
        HarnessState::Root
    }

    fn initial(&self) -> Self::State {
        HarnessState::Ready
    }

    /// The hierarchy is flat: every state sits directly under the root.
    fn superstate(&self, state: Self::State) -> Self::State {
        superstate_of(state)
    }

    fn is_terminal(&self, state: Self::State) -> bool {
        state.is_terminal()
    }

    fn all_states(&self) -> Vec<Self::State> {
        crate::state::ALL_STATES.to_vec()
    }

    fn dispatch_state(
        &mut self,
        state: Self::State,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<Self::State> {
        // Advancing's entry emits its advance signal (entry handlers never
        // transition; the kernel's executor bounces the EmitInternal back
        // as an internal event, which executes the step the walk points
        // at).
        if state == HarnessState::Advancing
            && matches!(event, Event::Internal(InternalEvent::Entry))
        {
            let run = self.run_state(ctx);
            return Reaction::Handled(vec![Self::advance_signal(&run.current)]);
        }
        let reaction = match state {
            HarnessState::Root => Reaction::Ignored,
            HarnessState::Ready => self.on_ready(event, ctx),
            HarnessState::Advancing => self.on_advancing(event, ctx),
            HarnessState::WaitingForModel => self.on_model(event, ctx),
            HarnessState::WaitingForTool => self.on_tool(event, ctx),
            HarnessState::WaitingForVerifier => self.on_verifier(event, ctx),
            HarnessState::WaitingForChild => self.on_child(event, ctx),
            HarnessState::Completed | HarnessState::Failed | HarnessState::Cancelled => {
                Reaction::Ignored
            }
        };
        // A transition's target is where the walk now rests; keep the
        // tracked leaf in step so snapshots name it.
        if let Reaction::Transition { target, .. } = &reaction {
            self.set_leaf(*target);
        }
        reaction
    }
}

impl HarnessMachine {
    // ── state handlers ──────────────────────────────────────────────────

    /// A message starts the run: seed the run state and point the walk at
    /// the entry step. Every later message is a tick (handled by the
    /// waiting states).
    fn on_ready(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        if !matches!(event, Event::UserMessage { .. }) {
            return Reaction::Ignored;
        }
        let mut state = self.run_state(ctx);
        state.start(self.run.plan(), self.run.inputs(), self.run.bundle_digest());
        state.set_run_values(self.run.run_values());
        if state.run_values().scratchpad.is_none() && plan_reads_scratchpad(self.run.plan()) {
            // The host decides where the run's private directory is; one that
            // gave none cannot run a workflow that writes into it.
            return self.fail_now(
                &mut state,
                "scratchpad_unavailable",
                "the workflow reads the run's scratchpad but this host provides none",
                ctx,
            );
        }
        state.delegate_depth = self.run.delegate_depth();
        state.ceiling = self.run.ceiling();
        state.store(ctx);
        Reaction::Transition {
            target: HarnessState::Advancing,
            effects: Vec::new(),
            rationale: "run started".into(),
        }
    }

    /// The walk is between side effects. Its entry emits the advance
    /// signal (the kernel's executor bounces it back as an internal
    /// event); the bounced event executes the step the walk points at and
    /// keeps executing pure steps inline until the walk parks on a side
    /// effect or rests terminal. A message is also an advance — a tick for
    /// a run that parked idle after its last side effect completed. A
    /// cancel moves to [`HarnessState::Cancelled`].
    fn on_advancing(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        if matches!(event, Event::UserCancelled) {
            return self.cancel(ctx);
        }
        let advance = match event {
            Event::Internal(InternalEvent::Custom { name, .. }) if name == ADVANCE_SIGNAL => true,
            Event::UserMessage { .. } => true,
            _ => false,
        };
        if !advance {
            return Reaction::Ignored;
        }
        let mut state = self.run_state(ctx);
        self.execute_current(&mut state, ctx)
    }

    /// A model turn is in flight (`predict`, or an `agent` loop's rounds).
    /// Nothing is in flight any more (the completion already stored and
    /// followed the `then` edge): a tick advances the walk.
    fn on_model(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        let mut run = self.run_state(ctx);
        if matches!(event, Event::UserCancelled) {
            // The turn executor ends a turn a watchdog cut short with the
            // same cancel a person's would be; it says why just before.
            return match run.watchdog.take() {
                Some(reason) if run.awaiting == Some(Waiting::Model) => {
                    self.on_turn_aborted(&mut run, &reason, ctx)
                }
                _ => self.cancel(ctx),
            };
        }
        if run.awaiting != Some(Waiting::Model) {
            return self.tick_when_idle(&mut run, event, ctx);
        }
        let Some(inflight) = run.inflight.clone() else {
            return Reaction::Ignored;
        };
        match event {
            Event::Internal(InternalEvent::Custom { name, payload }) if name == TURN_WATCHDOG => {
                run.watchdog = Some(payload.as_str().unwrap_or("a watchdog").to_string());
                run.store(ctx);
                Reaction::Ignored
            }
            Event::Internal(InternalEvent::Custom { name, .. }) if name == TURN_TRUNCATED => {
                run.truncated = true;
                run.store(ctx);
                Reaction::Ignored
            }
            Event::LlmTurnComplete {
                text, tool_calls, ..
            } => {
                if let Some(agent) = run.agent.clone() {
                    if agent.step == inflight.step {
                        return self.on_agent_turn(&mut run, agent, text, tool_calls, ctx);
                    }
                }
                if text.trim().is_empty() && !run.truncated {
                    return self.retry_turn(&mut run, &inflight.step, TurnTrouble::Empty, ctx);
                }
                run.turn_retries = 0;
                run.retry_delay_ms = 0;
                if std::mem::take(&mut run.truncated) {
                    return self.fail_now(
                        &mut run,
                        "output_truncated",
                        "the model's answer was cut off at its output-token limit",
                        ctx,
                    );
                }
                // `predict`: the turn's text is the answer. A schema'd
                // answer must parse as JSON (providers enforce the format,
                // but the caller always post-parses).
                let answer = match self.answer_value(&inflight.step, text) {
                    Some(v) => v,
                    None => return self.invalid_answer(&mut run, ctx),
                };
                self.complete_step(&mut run, &inflight.step, answer, ctx)
            }
            Event::LlmFailed { error } | Event::EffectFailed { error, .. } => {
                if self.restarts_interrupted(error) {
                    return self.restart_interrupted(&mut run, &inflight.step, ctx);
                }
                if error.starts_with(sven_hsm::TRANSIENT) {
                    return self.retry_turn(
                        &mut run,
                        &inflight.step,
                        TurnTrouble::Transient(error),
                        ctx,
                    );
                }
                if error.starts_with(sven_hsm::CONTEXT_EXHAUSTED) {
                    return self.fail_now(&mut run, "context_exhausted", error, ctx);
                }
                self.side_effect_failed(&mut run, error, ctx)
            }
            Event::ToolSucceeded {
                call_id,
                observation,
            } => {
                let answer = observation
                    .as_str()
                    .map_or_else(|| observation.to_string(), str::to_string);
                self.on_agent_tool_settled(&mut run, *call_id, &answer, ctx)
            }
            Event::ToolFailed { call_id, error } => {
                self.on_agent_tool_failed(&mut run, *call_id, error, ctx)
            }
            _ => self.on_gate_event(&mut run, event, ctx),
        }
    }

    /// A `tool` step's call result is in flight. Nothing in flight: a tick
    /// advances. (An agent loop's tool results arrive in
    /// [`HarnessState::WaitingForModel`].)
    fn on_tool(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        if matches!(event, Event::UserCancelled) {
            return self.cancel(ctx);
        }
        let mut run = self.run_state(ctx);
        if run.awaiting != Some(Waiting::Tool) {
            return self.tick_when_idle(&mut run, event, ctx);
        }
        let Some(inflight) = run.inflight.clone() else {
            return Reaction::Ignored;
        };
        match event {
            Event::ToolSucceeded {
                call_id: done,
                observation,
            } if Some(done) == inflight.call_id.as_ref() => {
                if self.is_gate_step(&inflight.step) {
                    return self.gate_settled(&mut run, &inflight.step, Ok(observation), ctx);
                }
                if self.is_iterate_step(&inflight.step) {
                    return self.snapshot_settled(&mut run, &inflight.step, Ok(observation), ctx);
                }
                self.complete_step(&mut run, &inflight.step, observation.clone(), ctx)
            }
            Event::ToolFailed {
                call_id: done,
                error,
            } if Some(done) == inflight.call_id.as_ref() => {
                if self.is_gate_step(&inflight.step) {
                    return self.gate_settled(&mut run, &inflight.step, Err(error), ctx);
                }
                if self.is_iterate_step(&inflight.step) {
                    return self.snapshot_settled(&mut run, &inflight.step, Err(error), ctx);
                }
                self.side_effect_failed(&mut run, error, ctx)
            }
            _ => self.on_gate_event(&mut run, event, ctx),
        }
    }

    /// A verifier verdict is in flight. Nothing in flight: a tick advances.
    fn on_verifier(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        if matches!(event, Event::UserCancelled) {
            return self.cancel(ctx);
        }
        let mut run = self.run_state(ctx);
        if run.awaiting != Some(Waiting::Verifier) {
            return self.tick_when_idle(&mut run, event, ctx);
        }
        let Some(inflight) = run.inflight.clone() else {
            return Reaction::Ignored;
        };
        match event {
            Event::VerificationComplete { verdict } => {
                // The verdict's own record: which verifier ran and what it
                // said, so a trace answers "which verification failed"
                // without re-reading the plan.
                if let Some(step) = self.run.plan().step(&inflight.step) {
                    if let PlanPrimitive::Verify { spec, .. } = &step.primitive {
                        let parsed: sven_vocab::verify::VerifierSpec =
                            serde_json::from_value(spec.clone())
                                .unwrap_or(sven_vocab::verify::VerifierSpec::Unsupported);
                        run.trace.push(TraceRecord::Verified {
                            verifier: verifier_word(&parsed).to_string(),
                            ok: matches!(verdict, VerifierVerdict::Passed),
                        });
                    }
                }
                self.complete_step(
                    &mut run,
                    &inflight.step,
                    Value::String(verdict_text(verdict)),
                    ctx,
                )
            }
            Event::EffectFailed { error, .. } => {
                if self.restarts_interrupted(error) {
                    return self.restart_interrupted(&mut run, &inflight.step, ctx);
                }
                self.side_effect_failed(&mut run, error, ctx)
            }
            _ => Reaction::Ignored,
        }
    }

    /// A delegated child run is in flight. Its completion arrives
    /// correlated by the machine id the `InstantiateSubmachine` effect
    /// carried; a completion for some other machine settles nothing. A
    /// failed effect (no spawner configured, the child run errored) fails
    /// the run like any other side effect.
    fn on_child(&mut self, event: &Event, ctx: &mut Context) -> Reaction<HarnessState> {
        if matches!(event, Event::UserCancelled) {
            return self.cancel(ctx);
        }
        let mut run = self.run_state(ctx);
        if run.awaiting != Some(Waiting::Child) {
            return self.tick_when_idle(&mut run, event, ctx);
        }
        let Some(child) = run.child.clone() else {
            return Reaction::Ignored;
        };
        match event {
            Event::Internal(InternalEvent::SubmachineCompleted { machine, result }) => {
                if *machine != child.machine {
                    // A completion for a child nobody waits on (a stale
                    // delivery from an earlier incarnation) is ignored.
                    return Reaction::Ignored;
                }
                // A version 2 child answers with an envelope; its usage is
                // charged to this run and the value it carries is the result.
                let (result, reported_ok) = crate::delegation::take_back(&mut run, result);
                let result = &result;
                run.trace.push(TraceRecord::ChildCompleted {
                    step: child.step.clone(),
                    child: child.machine.clone(),
                    // An absent `ok` is a child that returned normally; the child
                    // failure protocol is an explicit ok:false result.
                    ok: reported_ok,
                });
                run.child = None;
                let step = child.step;
                if !reported_ok {
                    // A child that failed fails this step: the result
                    // is kept for whoever inspects the run, but the walk does
                    // not continue.
                    let message = result
                        .get("message")
                        .or_else(|| result.get("error"))
                        .and_then(Value::as_str)
                        .unwrap_or("the delegated child reported failure")
                        .to_string();
                    run.store_step_value(&step, result.clone(), None);
                    return self.fail_now(&mut run, "child_failed", &message, ctx);
                }
                self.complete_step(&mut run, &step, result.clone(), ctx)
            }
            Event::EffectFailed { error, .. } => self.side_effect_failed(&mut run, error, ctx),
            // The tick rule: a run with I/O in flight does not advance its
            // pure chain, and re-executing a parked step would emit a
            // second child for the same delegate.
            _ => Reaction::Ignored,
        }
    }
}

/// A verdict's stored form: the word a template can address.
pub(super) fn verdict_text(verdict: &VerifierVerdict) -> String {
    match verdict {
        VerifierVerdict::Passed => "passed".to_string(),
        VerifierVerdict::Failed { reason } => format!("failed: {reason}"),
        VerifierVerdict::NeedsHuman { .. } => "needs_human".to_string(),
        VerifierVerdict::Unknown { reason } => format!("unknown: {reason}"),
    }
}

/// A primitive in words — the same word a `StepStarted` trace record and
/// its event carry.
pub(super) fn primitive_word(primitive: &PlanPrimitive) -> &'static str {
    match primitive {
        PlanPrimitive::Predict { .. } => "predict",
        PlanPrimitive::Agent { .. } => "agent",
        PlanPrimitive::Verify { .. } => "verify",
        PlanPrimitive::Assign { .. } => "assign",
        PlanPrimitive::Branch { .. } => "branch",
        PlanPrimitive::Delegate { .. } => "delegate",
        PlanPrimitive::Return { .. } => "return",
        PlanPrimitive::Fail { .. } => "fail",
        PlanPrimitive::Gate { .. } => "gate",
        PlanPrimitive::Iterate { .. } => "iterate",
    }
}

/// A verifier's kind in words — the identity a `Verified` record carries.
pub(super) fn verifier_word(spec: &sven_vocab::verify::VerifierSpec) -> &'static str {
    use sven_vocab::verify::VerifierSpec;
    match spec {
        VerifierSpec::FileExists { .. } => "file_exists",
        VerifierSpec::FileHash { .. } => "file_hash",
        VerifierSpec::JsonPredicate { .. } => "json_predicate",
        VerifierSpec::HttpPredicate { .. } => "http_predicate",
        VerifierSpec::AskHuman { .. } => "ask_human",
        VerifierSpec::All { .. } => "all",
        VerifierSpec::Any { .. } => "any",
        VerifierSpec::Unsupported => "unsupported",
    }
}

/// A primitive's `then` successor, if it has one.
pub(super) fn then_of(primitive: &PlanPrimitive) -> String {
    match primitive {
        PlanPrimitive::Predict { then, .. }
        | PlanPrimitive::Agent { then, .. }
        | PlanPrimitive::Verify { then, .. }
        | PlanPrimitive::Assign { then, .. }
        | PlanPrimitive::Delegate { then, .. } => then.clone(),
        PlanPrimitive::Gate { then, .. } | PlanPrimitive::Iterate { then, .. } => then.clone(),
        PlanPrimitive::Branch { .. }
        | PlanPrimitive::Return { .. }
        | PlanPrimitive::Fail { .. } => String::new(),
    }
}

/// Where a primitive stores its result, if it stores one.
pub(super) fn store_as_of(primitive: &PlanPrimitive) -> Option<String> {
    match primitive {
        PlanPrimitive::Predict { store_as, .. }
        | PlanPrimitive::Agent { store_as, .. }
        | PlanPrimitive::Verify { store_as, .. }
        | PlanPrimitive::Delegate { store_as, .. } => store_as.clone(),
        _ => None,
    }
}

/// A plan capability name's kernel bucket; unknown names grant nothing.
pub(super) fn capability_by_name(name: &str) -> Option<ToolCapability> {
    match name {
        "ReadFile" => Some(ToolCapability::ReadFile),
        "WriteFile" => Some(ToolCapability::WriteFile),
        "DeleteFile" => Some(ToolCapability::DeleteFile),
        "ExecuteShell" => Some(ToolCapability::ExecuteShell),
        "NetworkAccess" => Some(ToolCapability::NetworkAccess),
        "GitOperation" => Some(ToolCapability::GitOperation),
        "RunVerifier" => Some(ToolCapability::RunVerifier),
        "ControlDevice" => Some(ToolCapability::ControlDevice),
        "SpawnChild" => Some(ToolCapability::SpawnChild),
        _ => None,
    }
}

/// The content of `text` when it is exactly one markdown code fence (with or
/// without a language tag) and nothing else but whitespace. Local models wrap
/// requested JSON this way; any other surrounding text is not stripped, so an
/// answer that talks around its object is still refused.
fn unfenced(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix("```")?.strip_suffix("```")?;
    let (_tag, body) = inner.split_once('\n')?;
    Some(body.trim())
}
