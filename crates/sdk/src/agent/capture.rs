// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What an agent takes from the session it ran, once the run ends: the
//! conversation, the tools it was offered, and the records of what happened to
//! its calls.

use sven_hsm::ApprovalId;
use sven_machines::machines::loop_core::{LoopState, LOOP_STATE_KEY};
use sven_model::Message;

use super::Agent;
use crate::run::Decision;

impl Agent {
    /// Replaces the history folded from observations with the conversation
    /// the kernel actually holds: the thread the model reads from, minus the
    /// system message the session seeds. The observation broadcast can lag
    /// and drop events under load; the store cannot.
    ///
    /// A machine that keeps its conversation on threads of its own (`sdlc`
    /// runs one per phase) leaves the primary thread at its seed; its folded
    /// history is kept. Compactions of the primary thread are recorded so the
    /// trajectory can say the history is not the whole conversation.
    pub(super) fn take_history(&mut self, handle: &sven_bootstrap::RuntimeHandle, seeded: usize) {
        let thread = sven_machines::mode::primary_thread(&self.state.mode);
        let state = &mut self.state;
        let (held, compactions) = handle.read_conversation(|store| {
            let held: Vec<Message> = store
                .snapshot(thread)
                .into_iter()
                .filter(|m| m.role != sven_model::Role::System)
                .collect();
            let compactions: Vec<_> = store
                .compactions()
                .iter()
                .filter(|c| c.thread == thread)
                .cloned()
                .collect();
            state.unparsed_arguments.extend(
                store
                    .unparsed_arguments()
                    .iter()
                    .map(|(id, raw)| (id.clone(), raw.clone())),
            );
            state.absorb_call_records(store.gate_decisions(), store.memory_retrievals());
            (held, compactions)
        });

        // A compaction shrinks the thread, possibly below what the turn was
        // seeded with, so growth alone cannot tell a thread the machine used
        // from one it left at its seed.
        if held.len() > seeded || (!compactions.is_empty() && !held.is_empty()) {
            self.state.history = held;
            self.state.compactions.extend(compactions);
        }
    }

    /// Records the tools the model was offered, resolved exactly as the turn
    /// executor resolves them: the machine's named tools, else everything the
    /// registry has for the machine's mode. A kernel that never started a tool
    /// loop offered nothing new, so the previous record stands.
    pub(super) fn take_offered_tools(
        &mut self,
        handle: &sven_bootstrap::RuntimeHandle,
        no_tools: bool,
    ) {
        let Some(snapshot) = &self.state.kernel else {
            return;
        };
        if snapshot.context.fact(LOOP_STATE_KEY).is_none() {
            return;
        }
        let looping = LoopState::load(&snapshot.context);
        let schemas = if no_tools {
            Vec::new()
        } else {
            handle.tool_registry().schemas_offered(
                &looping.tools,
                &looping.all_tools_mode,
                &snapshot.context.active_tools,
            )
        };
        self.state.tools = schemas
            .iter()
            .map(|s| atif::function_tool(&s.name, &s.description, &s.parameters))
            .collect();
    }

    /// Records that a person decided the approval the agent was parked on,
    /// so the trajectory names the decision beside the call it was about.
    pub(super) fn record_parked_decision(
        &self,
        handle: &sven_bootstrap::RuntimeHandle,
        approval_id: ApprovalId,
        decision: &Decision,
    ) {
        let Some(pending) = self
            .state
            .kernel
            .as_ref()
            .and_then(|snapshot| snapshot.context.pending_approval.as_ref())
        else {
            return;
        };
        let (approved, reason) = match decision {
            Decision::Approve => (true, None),
            Decision::Deny { reason } => (false, Some(reason.clone())),
        };
        handle.record_human_decision(approval_id, pending.capability, approved, reason);
    }
}
