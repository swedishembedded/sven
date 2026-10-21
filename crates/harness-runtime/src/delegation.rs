// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Starting a child run and taking its result back.
//!
//! A `delegate` step starts a registered harness as a child. The descriptor
//! tells the child how deep it is and what its parent had left of its
//! budgets. The child answers with an envelope that says whether it succeeded,
//! carries what it returned or why it failed, and reports what it spent, which
//! the parent adds to its own counters.
//!
//! Swedish Embedded AB implements verifiable, declarative agent workflows for
//! its clients. If your team needs expertise in composing long-running agent
//! work from reviewed, reusable parts then you can procure our services by
//! sending an email to info@swedishembedded.com.

use serde_json::{json, Value};
use sven_harness_spec::{PlanPrimitive, PlanStep};
use sven_hsm::{Context, Effect, Reaction};

use crate::machine::HarnessMachine;
use crate::run::{child_machine_id, ChildRun, HarnessRunState, TraceRecord, Waiting, OUTCOME_FACT};
use crate::state::HarnessState;

/// Marks a child's result as an envelope.
pub const ENVELOPE: &str = "$envelope";
const ENVELOPE_VERSION: &str = "harness-result/1";

/// The result a finished harness child hands its parent: an envelope
/// `{ok, value | code, message, error, usage}`.
#[must_use]
pub fn child_result(completed: bool, ctx: &Context) -> Value {
    let outcome = ctx.fact(OUTCOME_FACT).cloned();
    let state = HarnessRunState::load(ctx);
    let usage = json!({
        "model_turns": state.counts.model_turns,
        "tool_calls": state.counts.tool_calls,
        "steps": state.counts.steps,
    });
    match (completed, outcome) {
        (true, Some(value)) => {
            json!({ ENVELOPE: ENVELOPE_VERSION, "ok": true, "value": value, "usage": usage })
        }
        (_, outcome) => {
            let text = |key: &str| {
                outcome
                    .as_ref()
                    .and_then(|o| o.get(key))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            };
            let message = text("message")
                .unwrap_or_else(|| "the harness child ended without an outcome".to_string());
            json!({
                ENVELOPE: ENVELOPE_VERSION,
                "ok": false,
                "code": text("code").unwrap_or_else(|| "no_outcome".to_string()),
                "message": message,
                "error": message,
                "usage": usage,
            })
        }
    }
}

/// What a parent keeps of a child's result: the usage is charged to the
/// parent, and an envelope gives way to the value it carries (or, for a
/// failure, `{ok: false, code, message, error}`). Whether the child reported
/// success comes back with it.
pub(crate) fn take_back(run: &mut HarnessRunState, result: &Value) -> (Value, bool) {
    if result.get(ENVELOPE).and_then(Value::as_str) != Some(ENVELOPE_VERSION) {
        let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(true);
        return (result.clone(), ok);
    }
    if let Some(usage) = result.get("usage") {
        run.absorb_usage(usage);
    }
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if ok {
        (result.get("value").cloned().unwrap_or(Value::Null), true)
    } else {
        let mut failure = result.clone();
        if let Some(map) = failure.as_object_mut() {
            map.remove(ENVELOPE);
            map.remove("usage");
        }
        (failure, false)
    }
}

impl HarnessMachine {
    /// A delegate step: starts the child and parks until it answers.
    pub(super) fn run_delegate(
        &mut self,
        run: &mut HarnessRunState,
        step: &PlanStep,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let step_id = step.id.clone();
        let PlanPrimitive::Delegate {
            ref target,
            ref input,
            ..
        } = step.primitive
        else {
            return self.fail_now(
                run,
                "internal",
                "delegate dispatch reached another step",
                ctx,
            );
        };
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
        let machine = child_machine_id(&self.run.plan().name, &step_id, None, run.counts.children);
        let mut descriptor = serde_json::Map::new();
        descriptor.insert("harness".into(), Value::String(target.clone()));
        descriptor.insert("input".into(), run.resolve_value(input));
        descriptor.insert("depth".into(), Value::from(depth));
        descriptor.insert("budget".into(), run.remaining(self.run.plan()).to_value());
        let effect = Effect::InstantiateSubmachine {
            machine,
            descriptor: Value::Object(descriptor),
        };
        self.cache_effect(effect.clone());
        self.park(run, step, Waiting::Child);
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
