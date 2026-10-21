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

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use sven_harness_spec::{Template, RESERVED_TOOLS};
use sven_hsm::{Context, Effect, ProposedToolCall, Reaction, ToolCallId};
use sven_vocab::{AnsweredCall, TurnRequest};

use crate::approval::HeldCall;
use crate::command_gate::TurnTrouble;
use crate::decoding::{system_text, TurnShape};
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Inflight, TraceRecord, Waiting};
use crate::state::HarnessState;

/// The agent-mode tool set an agent step without a tool list is offered.
const AGENT_MODE: &str = "agent";

/// The agent step's in-flight model↔tool loop.
///
/// Mirrors the machines' `LoopState` in miniature: a harness agent step has
/// a fixed tool allowlist, no all-tools fallback, and no schema demand —
/// the plan's agent primitive is exactly this loop and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentLoop {
    /// The agent step's id.
    pub step: String,
    /// The thread the loop runs in (`harness.<plan name>`).
    pub thread: String,
    /// The step's tool allowlist.
    pub tools: Vec<String>,
    /// Calls proposed by the last turn, not yet settled.
    pub pending: Vec<ToolCallId>,
    /// Model turns spent in this loop so far.
    pub rounds: u64,
    /// `true` when the step declared no tool list: the session's agent-mode
    /// tools are open to it and `tools` is empty. `false` when `tools` is
    /// an explicit allowlist, which is also enforced on proposed calls.
    #[serde(default)]
    pub open: bool,
    /// Proposed calls the machine answered itself (call id to the result
    /// text): refusals, and the variable tools it serves. Drained into the
    /// next turn, which appends them to the conversation.
    #[serde(default)]
    pub answered: BTreeMap<String, String>,
    /// The variables the step was granted, which the machine's variable
    /// tools serve.
    #[serde(default)]
    pub grants: BTreeMap<String, sven_harness_spec::vars::VarAccess>,
    /// The calls the last turn proposed and the step admitted (call id to
    /// call): what a call the permission gate holds for approval is run as
    /// once it is approved.
    #[serde(default)]
    pub calls: BTreeMap<String, HeldCall>,
    /// The model role that answers the loop's turns.
    #[serde(default)]
    pub role: Option<String>,
    /// Where the loop records how it ended.
    #[serde(default)]
    pub stop_as: Option<String>,
    /// The fact holding the calls the step may still make.
    #[serde(default)]
    pub pool: Option<String>,
    /// The repeated answers that end the loop.
    #[serde(default)]
    pub max_repeats: Option<u32>,
    /// How many times each (call, answer) pair has been seen this loop.
    #[serde(default)]
    pub repeats: BTreeMap<String, u32>,
    /// Why the loop ends once the calls in flight settle (`tool_cap`,
    /// `repetition`).
    #[serde(default)]
    pub ending: Option<String>,
    /// The decoding settings of the step's turns.
    #[serde(default)]
    pub decoding: sven_vocab::TurnDecoding,
}

impl HarnessMachine {
    /// The agent step's first turn: its prompt, with the tools the step
    /// declared (`Some`) or the session's agent-mode tools (`None`).
    pub(super) fn agent_turn_effect(
        &self,
        state: &HarnessRunState,
        prompt: &Template,
        tools: &Option<Vec<String>>,
        shape: TurnShape<'_>,
    ) -> Effect {
        let instruction = state.resolve_template(prompt);
        agent_request(
            TurnRequest {
                thread: shape.thread.to_string(),
                instruction: instruction.as_str().unwrap_or_default().to_string(),
                tools: tools.as_deref().unwrap_or_default().to_vec(),
                system: system_text(state, shape.system),
                origin: shape.origin.map(str::to_string),
                role: shape.role.map(str::to_string),
                decoding: shape.decoding,
                delay_ms: (state.retry_delay_ms > 0).then_some(state.retry_delay_ms),
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
        let answered = std::mem::take(&mut agent.answered)
            .into_iter()
            .map(|(call_id, content)| AnsweredCall { call_id, content })
            .collect();
        Some(agent_request(
            TurnRequest {
                thread: agent.thread.clone(),
                tools: agent.tools.clone(),
                answered_calls: answered,
                role: agent.role.clone(),
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
        if !tool_calls.is_empty() {
            run.truncated = false;
        }
        // An answer of nothing at all is not an answer: ask again (bounded),
        // unless the reply was cut at the output limit, which is its own
        // status below.
        if tool_calls.is_empty() && text.trim().is_empty() && !run.truncated {
            return self.retry_turn(run, &agent.step, TurnTrouble::Empty, ctx);
        }
        run.turn_retries = 0;
        run.retry_delay_ms = 0;
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
            let stop = if std::mem::take(&mut run.truncated) {
                "output_truncated"
            } else if sven_vocab::announces_unfinished_work(text) {
                "stopped_without_conclusion"
            } else {
                "concluded"
            };
            return self.end_agent_loop(run, &agent, stop, text, ctx);
        }

        // The gate tool belongs to the workflow's gates: no agent step is
        // admitted a call to it, whatever its allowlist says, so the model
        // can neither run a gate nor make one pass.
        let (admitted, refused): (Vec<&ProposedToolCall>, Vec<&ProposedToolCall>) =
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
            let answer = AnsweredCall::refused(call.call_id.as_uuid().to_string(), &reason);
            agent.answered.insert(answer.call_id, answer.content);
        }
        // The variable tools are answered here and now, by the machine: they
        // never become effects and draw on no allowance.
        let (served, rest): (Vec<&ProposedToolCall>, Vec<&ProposedToolCall>) = admitted
            .into_iter()
            .partition(|call| crate::vars::is_var_tool(&call.name) && !agent.grants.is_empty());
        let mut admitted = rest;
        for call in &served {
            let content =
                crate::vars::answer(self.plan(), run, &agent.grants, &call.name, &call.args);
            if call.name == "harness_vars_set" {
                run.trace.push(TraceRecord::VariableSet {
                    step: agent.step.clone(),
                    name: call
                        .args
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    stored: content.starts_with("ok:"),
                    answer: content.clone(),
                    preview: call
                        .args
                        .get("value")
                        .map(|v| match v {
                            Value::String(text) => text.chars().take(400).collect(),
                            other => other.to_string().chars().take(400).collect(),
                        })
                        .unwrap_or_default(),
                });
            }
            agent
                .answered
                .insert(call.call_id.as_uuid().to_string(), content);
        }
        run.counts.tool_calls += served.len() as u64;
        // A step with a pool of calls may make no more than the pool holds:
        // the calls beyond it are refused and the loop ends once the others
        // settle, with the stop `tool_cap`.
        if let Some(pool) = agent.pool.clone() {
            let left = run.facts.get(&pool).and_then(Value::as_u64);
            if let Some(left) = left {
                let allowed = usize::try_from(left).unwrap_or(usize::MAX);
                if admitted.len() > allowed {
                    for call in admitted.split_off(allowed) {
                        let answer = AnsweredCall::refused(
                            call.call_id.as_uuid().to_string(),
                            "the tool-call allowance for this turn is spent",
                        );
                        agent.answered.insert(answer.call_id, answer.content);
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
