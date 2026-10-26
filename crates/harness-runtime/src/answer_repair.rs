// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bounded repair of a schema'd `predict` answer that is truncated or does
//! not parse.
//!
//! One malformed answer must not cost a long run: the model is told what was
//! wrong in the same conversation and asked again, a bounded number of times.
//! When the repairs are spent the failure is the item's in a loop that
//! handles item failures, and the run's otherwise.
//!
//! Swedish Embedded AB implements agent workflows that survive unreliable
//! model output for its clients. If your team needs expertise in running
//! long unattended generation jobs then you can procure our services by
//! sending an email to info@swedishembedded.com.

use serde_json::Value;
use sven_harness_spec::{PlanPrimitive, Template, DEFAULT_ANSWER_REPAIRS, ITEM_FAILED_FACT};
use sven_hsm::{Context, Reaction};

use crate::decoding::{conversation_thread, resolve_decoding, TurnShape};
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, TraceRecord};
use crate::state::HarnessState;

/// What is wrong with a schema'd answer.
enum Problem {
    /// The reply was cut off at the output-token limit.
    Truncated,
    /// The reply is not JSON; the parser's reason.
    Unparseable(String),
}

impl Problem {
    /// The failure code the run (or the item) ends with.
    fn code(&self) -> &'static str {
        match self {
            Self::Truncated => "output_truncated",
            Self::Unparseable(_) => "invalid_answer",
        }
    }

    /// The failure message once `repairs` repair turns did not help.
    fn failure(&self, repairs: u32) -> String {
        let spent = match repairs {
            0 => String::new(),
            n => format!(" after {n} repair attempt(s)"),
        };
        match self {
            Self::Truncated => {
                format!("the model's answer was cut off at its output-token limit{spent}")
            }
            Self::Unparseable(why) => format!("answer is not valid JSON{spent}: {why}"),
        }
    }

    /// What the model is told in the repair turn.
    fn prompt(&self) -> String {
        match self {
            Self::Truncated => "Your previous answer was cut off at the output-token limit, so \
                it is incomplete. Answer again with the complete JSON document and nothing \
                else; keep it shorter and think less."
                .to_string(),
            Self::Unparseable(why) => format!(
                "Your previous answer is not valid JSON ({why}). Answer again with only the \
                 JSON document the task asks for: no commentary around it."
            ),
        }
    }
}

impl HarnessMachine {
    /// A schema'd predict turn came back: its answer completes the step, or
    /// earns a repair turn, or ends the item or the run.
    pub(super) fn on_schema_answer(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        text: &str,
        truncated: bool,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let problem = if truncated {
            Problem::Truncated
        } else {
            match self.answer_value(step_id, text) {
                Ok(answer) => {
                    run.answer_repairs = 0;
                    return self.complete_step(run, step_id, answer, ctx);
                }
                Err(why) => Problem::Unparseable(why),
            }
        };
        self.repair_or_give_up(run, step_id, &problem, ctx)
    }

    fn repair_or_give_up(
        &mut self,
        run: &mut HarnessRunState,
        step_id: &str,
        problem: &Problem,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(PlanPrimitive::Predict {
            schema,
            conversation,
            system,
            role,
            decoding,
            repair_attempts,
            repair_limit_input,
            on_unusable_answer,
            ..
        }) = self.plan().step(step_id).map(|s| s.primitive.clone())
        else {
            return self.fail_now(
                run,
                "internal",
                "a repair reached a step that is no predict",
                ctx,
            );
        };
        let mut limit = repair_attempts.unwrap_or(DEFAULT_ANSWER_REPAIRS);
        if let Some(name) = &repair_limit_input {
            let hole = Template::parse(&format!("{{{{ inputs.{name} }}}}")).ok();
            let allowed = hole
                .map(|t| run.resolve_template(&t))
                .and_then(|v| match v {
                    Value::String(text) => text.trim().parse::<u64>().ok(),
                    other => other.as_u64(),
                });
            limit = limit.min(u32::try_from(allowed.unwrap_or(0)).unwrap_or(u32::MAX));
        }
        if run.answer_repairs >= limit {
            let message = problem.failure(std::mem::take(&mut run.answer_repairs));
            let Some(target) = on_unusable_answer else {
                return self.fail_now(run, problem.code(), &message, ctx);
            };
            run.facts
                .insert(ITEM_FAILED_FACT.to_string(), Value::String(message));
            run.awaiting = None;
            run.inflight = None;
            return self.walk_to(run, &target, ctx);
        }
        let decoding = match resolve_decoding(run, &decoding) {
            Ok(d) => d,
            Err(why) => return self.fail_now(run, "invalid_setting", &why, ctx),
        };
        run.answer_repairs += 1;
        run.counts.model_turns += 1;
        if run.exceeds_model_budget(self.plan()) {
            return self.fail_now(
                run,
                "model_turn_budget",
                "the run exceeded its model-turn budget",
                ctx,
            );
        }
        run.trace.push(TraceRecord::TurnRetried {
            step: step_id.to_string(),
            reason: problem.code().to_string(),
            attempt: u64::from(run.answer_repairs),
        });
        let thread = conversation_thread(self.plan(), run, conversation.as_ref());
        let effect = self.turn_effect_text(
            run,
            problem.prompt(),
            schema.as_deref(),
            TurnShape {
                thread: &thread,
                system: system.as_ref(),
                origin: Some("repair"),
                role: role.as_deref(),
                decoding,
            },
        );
        self.cache_effect(effect.clone());
        run.store(ctx);
        Reaction::Handled(vec![effect])
    }
}
