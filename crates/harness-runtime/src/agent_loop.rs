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
use sven_harness_spec::Template;
use sven_hsm::{Context, Effect, ProposedToolCall, Reaction, ToolCallId};
use sven_vocab::{RefusedCall, TurnRequest};

use crate::approval::HeldCall;
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
    ) -> Effect {
        let instruction = state.resolve_template(prompt);
        agent_request(
            harness_thread(self.plan()),
            instruction.as_str().unwrap_or_default().to_string(),
            tools.as_deref().unwrap_or_default().to_vec(),
            tools.is_none(),
            Vec::new(),
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
            agent.thread.clone(),
            String::new(),
            agent.tools.clone(),
            agent.open,
            refused,
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
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(agent) = run.agent.as_mut() else {
            return Reaction::Ignored;
        };
        if !agent.pending.contains(&done) {
            return Reaction::Ignored;
        }
        agent.pending.retain(|id| *id != done);
        if !agent.pending.is_empty() {
            run.store(ctx);
            return Reaction::handled();
        }
        let effect = self.continuation_effect(run);
        run.store(ctx);
        self.cache_effect_opt(effect.clone());
        Reaction::Handled(effect.into_iter().collect())
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
            run.agent = None;
            run.awaiting = None;
            run.inflight = None;
            let step = agent.step.clone();
            return self.complete_step(run, &step, Value::String(text.to_string()), ctx);
        }

        let (admitted, refused): (Vec<&ProposedToolCall>, Vec<&ProposedToolCall>) = tool_calls
            .iter()
            .partition(|call| agent.open || agent.tools.contains(&call.name));
        for call in refused {
            agent.refused.insert(
                call.call_id.as_uuid().to_string(),
                format!("tool `{}` is not in this step's allowlist", call.name),
            );
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
/// machine owns the loop).
fn agent_request(
    thread: String,
    instruction: String,
    tools: Vec<String>,
    all_tools: bool,
    refused_calls: Vec<RefusedCall>,
) -> Effect {
    let request = TurnRequest {
        thread,
        instruction,
        tools,
        all_tools_mode: if all_tools { AGENT_MODE } else { "" }.to_string(),
        active_tools: Vec::new(),
        schema: Value::Null,
        schema_name: String::new(),
        model: None,
        dynamic_suffix: None,
        max_tool_rounds: Some(0),
        refused_calls,
    };
    Effect::CallLlm {
        request: request.to_value(),
    }
}
