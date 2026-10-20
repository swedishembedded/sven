// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The `agent` step's model/tool loop.
//!
//! An agent step offers the model its declared tools on every turn (or the
//! session's agent-mode tools when it declares none). A declared list is
//! also an allowlist: a proposed call outside it is never turned into an
//! effect, and the model is told why on its next turn.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use serde_json::Value;
use sha2::Digest;
use sven_harness_spec::{Template, RESERVED_TOOLS};
use sven_hsm::{Context, Effect, ProposedToolCall, Reaction, ToolCallId};
use sven_vocab::{RefusedCall, TurnRequest};

use crate::approval::HeldCall;
use crate::decoding::system_text;
use crate::machine::{harness_thread, HarnessMachine};
use crate::run::{AgentLoop, HarnessRunState, Inflight, Waiting};
use crate::state::HarnessState;

/// The agent-mode tool set an agent step without a tool list is offered.
const AGENT_MODE: &str = "agent";

impl HarnessMachine {
    /// The agent step's first turn: its prompt, with the tools the step
    /// declared (`Some`) or the session's agent-mode tools (`None`).
    pub(super) fn agent_turn_effect(
        &self,
        state: &HarnessRunState,
        prompt: &Template,
        tools: &Option<Vec<String>>,
        system: Option<&Template>,
        origin: Option<&str>,
        decoding: sven_vocab::TurnDecoding,
    ) -> Effect {
        let instruction = state.resolve_template(prompt);
        agent_request(
            TurnRequest {
                thread: harness_thread(self.plan()),
                instruction: instruction.as_str().unwrap_or_default().to_string(),
                tools: tools.as_deref().unwrap_or_default().to_vec(),
                system: system_text(state, system),
                origin: origin.map(str::to_string),
                decoding,
                ..TurnRequest::default()
            },
            tools.is_none(),
        )
    }

    /// The loop's continuation turn: the next model round after the
    /// proposed tools settled. Drains the refusals recorded since the last
    /// turn into the request.
    pub(super) fn continuation_effect(&self, state: &mut HarnessRunState) -> Option<Effect> {
        let agent = state.agent.as_mut()?;
        let refused = std::mem::take(&mut agent.refused)
            .into_iter()
            .map(|(call_id, reason)| RefusedCall { call_id, reason })
            .collect();
        Some(agent_request(
            TurnRequest {
                thread: agent.thread.clone(),
                tools: agent.tools.clone(),
                refused_calls: refused,
                decoding: agent.decoding,
                ..TurnRequest::default()
            },
            agent.open,
        ))
    }

    /// A proposed call of the agent loop settled (succeeded or failed - the
    /// model reads either from its thread). When the last pending call is
    /// in, the loop's next model turn is requested. A result for a call the
    /// loop is not waiting on settles nothing.
    pub(super) fn on_agent_tool_settled(
        &mut self,
        run: &mut HarnessRunState,
        done: ToolCallId,
        answer: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(agent) = run.agent.as_mut() else {
            return Reaction::Ignored;
        };
        if !agent.pending.contains(&done) {
            return Reaction::Ignored;
        }
        agent.pending.retain(|id| *id != done);
        // The same call answered the same way again and again is a loop the
        // model will not leave by itself.
        if let (Some(limit), Some(call)) = (
            agent.max_repeats,
            agent.calls.get(&done.as_uuid().to_string()),
        ) {
            let key = format!(
                "{:x}",
                sha2::Sha256::digest(format!("{}\0{}\0{answer}", call.name, call.args))
            );
            let seen = agent.repeats.entry(key).or_insert(0);
            *seen += 1;
            if *seen >= limit {
                agent.ending = Some("repetition".to_string());
            }
        }
        if !agent.pending.is_empty() {
            run.store(ctx);
            return Reaction::handled();
        }
        if let Some(stop) = agent.ending.clone() {
            let agent = agent.clone();
            return self.end_agent_loop(run, &agent, &stop, "", ctx);
        }
        let effect = self.continuation_effect(run);
        run.store(ctx);
        self.cache_effect_opt(effect.clone());
        Reaction::Handled(effect.into_iter().collect())
    }

    /// Ends the loop: records how it ended where the step asked, and
    /// completes the step with `text`.
    fn end_agent_loop(
        &mut self,
        run: &mut HarnessRunState,
        agent: &AgentLoop,
        stop: &str,
        text: &str,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        if let Some(name) = &agent.stop_as {
            run.facts
                .insert(name.clone(), Value::String(stop.to_string()));
        }
        run.agent = None;
        run.awaiting = None;
        run.inflight = None;
        self.complete_step(run, &agent.step, Value::String(text.to_string()), ctx)
    }

    /// One turn of an agent step's model/tool loop: admitted tool proposals
    /// run as `CallTool` effects (the loop stays in
    /// [`HarnessState::WaitingForModel`] and receives the results there, via
    /// the tool-settling arm), proposals outside the step's allowlist are
    /// refused, and a text-only turn is the loop's final answer.
    pub(super) fn on_agent_turn(
        &mut self,
        run: &mut HarnessRunState,
        mut agent: AgentLoop,
        text: &str,
        tool_calls: &[ProposedToolCall],
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        agent.rounds += 1;
        if let Some(budget) = self
            .run
            .plan()
            .limits
            .as_ref()
            .and_then(|l| l.max_model_turns)
        {
            if agent.rounds > budget {
                return self.fail_now(
                    run,
                    "model_turn_budget",
                    "agent loop exceeded its model-turn budget",
                    ctx,
                );
            }
        }
        if tool_calls.is_empty() {
            let stop = if sven_vocab::announces_unfinished_work(text) {
                "stopped_without_conclusion"
            } else {
                "concluded"
            };
            return self.end_agent_loop(run, &agent, stop, text, ctx);
        }

        // The gate tool belongs to the workflow's gates: no agent step is
        // admitted a call to it, whatever its allowlist says, so the model
        // can neither run a gate nor make one pass.
        let (mut admitted, refused): (Vec<&ProposedToolCall>, Vec<&ProposedToolCall>) =
            tool_calls.iter().partition(|call| {
                !RESERVED_TOOLS.contains(&call.name.as_str())
                    && (agent.open || agent.tools.contains(&call.name))
            });
        for call in refused {
            let reason = if RESERVED_TOOLS.contains(&call.name.as_str()) {
                format!(
                    "tool `{}` is reserved to the workflow's own steps",
                    call.name
                )
            } else {
                format!("tool `{}` is not in this step's allowlist", call.name)
            };
            agent
                .refused
                .insert(call.call_id.as_uuid().to_string(), reason);
        }
        // A step with a pool of calls may make no more than the pool holds:
        // the calls beyond it are refused and the loop ends once the others
        // settle, with the stop `tool_cap`.
        if let Some(pool) = agent.pool.clone() {
            let left = run.facts.get(&pool).and_then(Value::as_u64);
            if let Some(left) = left {
                let allowed = usize::try_from(left).unwrap_or(usize::MAX);
                if admitted.len() > allowed {
                    for call in admitted.split_off(allowed) {
                        agent.refused.insert(
                            call.call_id.as_uuid().to_string(),
                            "the tool-call allowance for this turn is spent".to_string(),
                        );
                    }
                    agent.ending = Some("tool_cap".to_string());
                }
                run.facts.insert(
                    pool,
                    Value::from(left.saturating_sub(admitted.len() as u64)),
                );
            }
        }
        if admitted.is_empty() {
            if let Some(stop) = agent.ending.clone() {
                return self.end_agent_loop(run, &agent, &stop, text, ctx);
            }
        }
        let effects: Vec<Effect> = admitted
            .iter()
            .map(|call| Effect::CallTool {
                call_id: call.call_id,
                name: call.name.clone(),
                capability: call.capability,
                args: call.args.clone(),
            })
            .collect();
        agent.pending = admitted.iter().map(|call| call.call_id).collect();
        agent.calls = admitted
            .iter()
            .map(|call| {
                let held = HeldCall {
                    name: call.name.clone(),
                    capability: call.capability,
                    args: call.args.clone(),
                };
                (call.call_id.as_uuid().to_string(), held)
            })
            .collect();
        run.counts.tool_calls += admitted.len() as u64;
        // Still Waiting::Model: the loop's tool results arrive in the same
        // state through the tool-settling arm.
        run.awaiting = Some(Waiting::Model);
        run.inflight = Some(Inflight {
            step: agent.step.clone(),
            call_id: None,
        });
        run.agent = Some(agent);
        if effects.is_empty() {
            // Nothing will run, so no tool result will ever re-open the
            // loop: tell the model at once.
            let effect = self.continuation_effect(run);
            run.store(ctx);
            self.cache_effect_opt(effect.clone());
            return Reaction::Handled(effect.into_iter().collect());
        }
        run.store(ctx);
        Reaction::Handled(effects)
    }
}

/// One agent-loop `CallLlm`: no schema, no tool rounds inside the turn (the
/// machine owns the loop). `turn` carries what differs from turn to turn.
fn agent_request(turn: TurnRequest, all_tools: bool) -> Effect {
    let request = TurnRequest {
        all_tools_mode: if all_tools { AGENT_MODE } else { "" }.to_string(),
        max_tool_rounds: Some(0),
        ..turn
    };
    Effect::CallLlm {
        request: request.to_value(),
    }
}
