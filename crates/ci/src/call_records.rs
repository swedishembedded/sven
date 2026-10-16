// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements headless agent runners whose recorded
// trajectories can be audited and learned from, for its clients. If your team
// needs expertise in unattended agent pipelines you can procure our services
// by sending an email to info@swedishembedded.com.

//! The records of what happened to a run's calls, carried from the sessions
//! that made them into the trajectory the runner writes.
//!
//! The gate's decisions and the memory a call read are kept in the session's
//! conversation store, not announced as events a surface might miss, so a
//! runner reads them from the session once its turn has run. A session lasts
//! one turn, so the runner keeps what each turn left.

use atif::{GateDecision, MemoryRetrieval};
use sven_bootstrap::RuntimeHandle;
use sven_session_store::StepAssembler;

/// Gate decisions and memory retrievals of the calls a run has made.
#[derive(Debug, Default, Clone)]
pub(crate) struct CallRecords {
    decisions: Vec<GateDecision>,
    retrievals: Vec<MemoryRetrieval>,
}

impl CallRecords {
    /// What the session behind `handle` recorded.
    pub(crate) fn of(handle: &RuntimeHandle) -> Self {
        Self {
            decisions: handle.gate_decisions(),
            retrievals: handle.memory_retrievals(),
        }
    }

    /// Adds what a later turn recorded. A decision replaces an earlier one
    /// about the same call.
    pub(crate) fn absorb(&mut self, later: Self) {
        for decision in later.decisions {
            match self
                .decisions
                .iter_mut()
                .find(|known| known.call_id == decision.call_id)
            {
                Some(known) => *known = decision,
                None => self.decisions.push(decision),
            }
        }
        self.retrievals.extend(later.retrievals);
    }

    /// Hands the records to `assembler`, which files each under the step that
    /// made the call, whether that step is open or already closed.
    pub(crate) fn feed(&self, assembler: &mut StepAssembler) {
        for decision in &self.decisions {
            assembler.push_gate_decision(decision.clone());
        }
        for retrieval in &self.retrievals {
            assembler.push_memory_retrieval(retrieval.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use atif::{GateAuthority, GateOutcome, GateRecord};
    use sven_model::{FunctionCall, Message, MessageContent, Role};

    use super::*;

    fn decision(call_id: &str, outcome: GateOutcome, decided_by: GateAuthority) -> GateDecision {
        GateDecision {
            decision_id: format!("decision-{call_id}"),
            call_id: call_id.into(),
            capability: "ExecuteShell".into(),
            policy: "requires_approval".into(),
            outcome,
            decided_by,
            reason: None,
            violations: Vec::new(),
        }
    }

    /// A user turn, then an agent step whose call `call_a` was answered, then
    /// another user turn: the step with the call is already closed.
    fn closed_run() -> StepAssembler {
        let mut assembler = StepAssembler::new();
        assembler.push_message(&Message::user("go"));
        assembler.push_message(&Message {
            role: Role::Assistant,
            content: MessageContent::ToolCall {
                tool_call_id: "call_a".into(),
                function: FunctionCall {
                    name: "shell".into(),
                    arguments: "{}".into(),
                },
            },
        });
        assembler.push_message(&Message::tool_result("call_a", "ok"));
        assembler.push_message(&Message::user("next"));
        assembler
    }

    #[test]
    fn ci_runner_trajectory_keeps_gate_records() {
        let mut turn_one = CallRecords::default();
        turn_one.decisions.push(decision(
            "call_a",
            GateOutcome::Denied,
            GateAuthority::HandlerDropped,
        ));
        let mut run = CallRecords::default();
        run.absorb(turn_one);
        // A later turn's session knows nothing of the first turn's calls.
        run.absorb(CallRecords::default());

        let mut assembler = closed_run();
        run.feed(&mut assembler);
        let steps = assembler.finish();

        let record = GateRecord::from_extra(steps[1].extra.as_ref().expect("gate record"))
            .expect("a well-formed gate record");
        assert_eq!(record.decisions[0].call_id, "call_a");
        assert_eq!(
            record.decisions[0].decided_by,
            GateAuthority::HandlerDropped
        );
    }

    #[test]
    fn a_later_decision_about_a_call_replaces_the_earlier_one() {
        let mut run = CallRecords::default();
        run.decisions.push(decision(
            "call_a",
            GateOutcome::Denied,
            GateAuthority::Policy,
        ));
        let mut later = CallRecords::default();
        later.decisions.push(decision(
            "call_a",
            GateOutcome::Approved,
            GateAuthority::Human,
        ));
        run.absorb(later);
        assert_eq!(run.decisions.len(), 1);
        assert_eq!(run.decisions[0].outcome, GateOutcome::Approved);
    }
}
