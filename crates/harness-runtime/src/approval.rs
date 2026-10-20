// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The approval flow of a harness run.
//!
//! Under manual approval the kernel's permission gate holds a tool call and
//! reports it to the machine ([`Event::ToolApprovalRequired`]). A machine
//! that ignored the report would leave the run waiting for a result that
//! never comes, so the harness machine owns the flow: it asks the person
//! (`RequestHumanApproval`, naming the held call), issues the call again once
//! it is approved - the gate then lets exactly that call through - and
//! settles it as a refusal when it is not. The call is a `tool` step's or an
//! `agent` loop's; either way the step's own failure or refusal handling
//! takes over from there.
//!
//! One person is asked about one call at a time: the kernel context holds a
//! single pending approval, so gates held while another is being asked wait
//! in the run state, in the order the gate reported them.
//!
//! Swedish Embedded AB implements programmable, data-defined agent harnesses
//! for its clients. If your team needs expertise in declarative agent
//! workflow languages then you can procure our services by sending an email
//! to info@swedishembedded.com.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sven_harness_spec::{IterateSource, PlanIteration, PlanPrimitive, GATE_TOOL, SNAPSHOT_TOOL};
use sven_hsm::context::PendingApproval;
use sven_hsm::ids::{ApprovalId, ToolCallId};
use sven_hsm::{
    capability_for_tool_name, Context, Effect, Event, GatedCall, Reaction, ToolCapability,
};

use crate::command_gate::{gate_args, GATE_CAPABILITY};
use crate::iteration::snapshot_args;
use crate::machine::HarnessMachine;
use crate::run::{HarnessRunState, Waiting};
use crate::state::HarnessState;

/// Why a call a person did not approve did not run.
const NOT_APPROVED: &str = "a human did not approve it";

/// A tool call as it was issued: what a held call is run as once approved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldCall {
    /// The tool's name.
    pub name: String,
    /// The capability the call was authorized as.
    pub capability: ToolCapability,
    /// The call's arguments.
    pub args: Value,
}

/// A call the permission gate held for a person, as the gate reported it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldGate {
    /// The held call.
    pub call_id: ToolCallId,
    /// The capability bucket that needs approval.
    pub capability: ToolCapability,
    /// What the person is asked to approve.
    pub description: String,
}

/// The approval id of the gate on `call_id`: derived from the call, never
/// minted fresh, so a replay of the recorded events reproduces it.
fn approval_id_of(call_id: ToolCallId) -> ApprovalId {
    ApprovalId::from_uuid(call_id.as_uuid())
}

impl HarnessMachine {
    /// Routes the approval events of a state with a tool call in flight; any
    /// other event settles nothing.
    pub(super) fn on_gate_event(
        &mut self,
        run: &mut HarnessRunState,
        event: &Event,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        match event {
            Event::ToolApprovalRequired {
                call_id,
                capability,
                description,
            } => {
                let gate = HeldGate {
                    call_id: *call_id,
                    capability: *capability,
                    description: description.clone(),
                };
                self.on_gate_held(run, gate, ctx)
            }
            Event::HumanApproved { approval_id } => {
                self.on_gate_answered(run, *approval_id, true, ctx)
            }
            Event::HumanRejected { approval_id, .. } => {
                self.on_gate_answered(run, *approval_id, false, ctx)
            }
            _ => Reaction::Ignored,
        }
    }

    /// The call `call_id` stands for, while it is in flight: the `tool`
    /// step's own call, or one the agent loop's last turn proposed.
    fn held_call(&self, run: &HarnessRunState, call_id: ToolCallId) -> Option<HeldCall> {
        if let Some(agent) = &run.agent {
            if let Some(call) = agent.calls.get(&call_id.as_uuid().to_string()) {
                return Some(call.clone());
            }
        }
        let inflight = run.inflight.as_ref()?;
        if run.awaiting != Some(Waiting::Tool) || inflight.call_id != Some(call_id) {
            return None;
        }
        match &self.plan().step(&inflight.step)?.primitive {
            PlanPrimitive::Tool { name, args, .. } => Some(HeldCall {
                name: name.clone(),
                capability: capability_for_tool_name(name),
                args: run.resolve_value(args),
            }),
            PlanPrimitive::Iterate {
                iteration:
                    PlanIteration::ForEach {
                        source: IterateSource::Json(json),
                        ..
                    },
                ..
            } => Some(HeldCall {
                name: SNAPSHOT_TOOL.to_string(),
                capability: ToolCapability::ReadFile,
                args: snapshot_args(json),
            }),
            PlanPrimitive::Gate {
                command,
                timeout_secs,
                cwd,
                stdin,
                ..
            } => Some(HeldCall {
                name: GATE_TOOL.to_string(),
                capability: GATE_CAPABILITY,
                args: gate_args(run, command, *timeout_secs, cwd.as_deref(), stdin.as_ref()),
            }),
            _ => None,
        }
    }

    /// The permission gate held a call for a person: ask now, or queue
    /// behind the call being asked about. A gate on a call the run is not
    /// waiting on settles nothing.
    pub(super) fn on_gate_held(
        &mut self,
        run: &mut HarnessRunState,
        gate: HeldGate,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        if self.held_call(run, gate.call_id).is_none() {
            return Reaction::Ignored;
        }
        run.approvals.push(gate);
        let effects = if run.approvals.len() == 1 {
            self.ask_front(run, ctx)
        } else {
            Vec::new()
        };
        run.store(ctx);
        Reaction::Handled(effects)
    }

    /// The person answered the approval being asked. An answer to any other
    /// approval settles nothing.
    pub(super) fn on_gate_answered(
        &mut self,
        run: &mut HarnessRunState,
        approval_id: ApprovalId,
        approved: bool,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        let Some(front) = run.approvals.first() else {
            return Reaction::Ignored;
        };
        if approval_id_of(front.call_id) != approval_id {
            return Reaction::Ignored;
        }
        let gate = run.approvals.remove(0);
        let reaction = if approved {
            ctx.approve(approval_id);
            let issue = self
                .held_call(run, gate.call_id)
                .map(|call| Effect::CallTool {
                    call_id: gate.call_id,
                    name: call.name,
                    capability: call.capability,
                    args: call.args,
                });
            run.store(ctx);
            Reaction::Handled(issue.into_iter().collect())
        } else {
            self.settle_refused(run, gate.call_id, ctx)
        };
        // The next held call is asked about next, whichever way this one went.
        match reaction {
            Reaction::Handled(mut effects) => {
                effects.extend(self.ask_front(run, ctx));
                run.store(ctx);
                Reaction::Handled(effects)
            }
            other => other,
        }
    }

    /// A call a person did not approve never runs: a `tool` step fails like
    /// any failed call, an agent loop's call settles as a refusal its model
    /// reads on the next turn.
    fn settle_refused(
        &mut self,
        run: &mut HarnessRunState,
        call_id: ToolCallId,
        ctx: &mut Context,
    ) -> Reaction<HarnessState> {
        match run.agent.as_mut() {
            Some(agent) => {
                agent
                    .refused
                    .insert(call_id.as_uuid().to_string(), NOT_APPROVED.to_string());
                self.on_agent_tool_settled(run, call_id, ctx)
            }
            None => self.side_effect_failed(run, NOT_APPROVED, ctx),
        }
    }

    /// Asks about the oldest held call, if any: records it as the context's
    /// pending approval and names the call to the person.
    fn ask_front(&self, run: &HarnessRunState, ctx: &mut Context) -> Vec<Effect> {
        let Some(gate) = run.approvals.first() else {
            return Vec::new();
        };
        let approval_id = approval_id_of(gate.call_id);
        ctx.set_pending_approval(PendingApproval {
            approval_id,
            capability: gate.capability,
            description: gate.description.clone(),
            call_id: Some(gate.call_id),
        });
        let call = self.held_call(run, gate.call_id).map(|call| GatedCall {
            name: call.name,
            args: call.args,
        });
        vec![Effect::RequestHumanApproval {
            approval_id,
            capability: gate.capability,
            description: gate.description.clone(),
            call,
        }]
    }
}
