// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Retry handling: what a failed side effect does inside (or outside) a
//! retry frame.
//!
//! A failure with a frame open loops back into the frame's body while
//! attempts remain, and continues at the retry step's `on_exhausted` when
//! they are spent. A failure with no frame fails the run.

use sven_harness_spec::{PlanPrimitive, PlanStep};
use sven_hsm::{capability_for_tool_name, Context, Effect, Reaction};

use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

impl HarnessMachine {
    /// A step failed with `code`: re-enter the retry body while attempts
    /// remain, continue at `on_exhausted` when they are spent, or fail the
    /// run when no retry frame is open.
    pub(super) fn step_failed(
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
            return match self.run.plan().step(&frame.step).map(|s| &s.primitive) {
                Some(PlanPrimitive::Retry { on_exhausted, .. }) => {
                    let target = on_exhausted.clone();
                    self.walk_to(run, &target, ctx)
                }
                _ => self.fail_now(run, code, error, ctx),
            };
        }
        let mut frame = frame;
        frame.left -= 1;
        let body = frame.body.clone();
        let attempt = frame.attempt();
        let remaining = frame.left - 1;
        run.retry = Some(frame);
        run.trace.push(TraceRecord::RetryAttempt {
            step: body.clone(),
            attempt,
            remaining,
        });
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
                // A re-entered body is a step start like any other body's.
                if let Some(spent) = self.begin_step(run, step, ctx) {
                    return spent;
                }
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
}
