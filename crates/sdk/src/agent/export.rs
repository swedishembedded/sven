// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements agent trajectory capture for its clients. If
// your team needs expertise in turning agent runs into training data then you
// can procure our services by sending an email to info@swedishembedded.com.
//! An agent's conversation as an ATIF document.

use super::Agent;

impl Agent {
    /// What the agent did, as an ATIF trajectory: every message, every tool
    /// call with its result, the model and the tools it was offered.
    ///
    /// The record is the whole conversation as it happened. A context
    /// compaction shrinks what the model reads next, not what happened, so
    /// every step before it stays, and the compaction is a `System` step
    /// carrying `extra.context_management` where it fired. `tool_definitions`
    /// are the schemas the last step's model requests carried, not every tool
    /// the engine has registered. A tool call whose arguments the model sent as
    /// invalid JSON has `{}` arguments and `extra.raw_arguments` with the text,
    /// flagged by `extra.unparsed_arguments`.
    ///
    /// Folded from the run's own observations and checked against the
    /// conversation the kernel holds: when events were dropped the kernel's
    /// thread, which holds only what survived each compaction, is the record
    /// and the requests of that run are in the totals but on no step. Each step
    /// carries the tokens, cost, serving model, seed, prompt and sampled token
    /// ids and serving identity of the model requests it covers, and the final
    /// metrics total every request the agent has made. A resumed agent exports
    /// the same document.
    /// Carries no reward: whether the work was right is for a
    /// verifier to say, not the agent.
    #[must_use]
    pub fn trajectory(&self) -> atif::Trajectory {
        self.trajectory_with(&[], &[])
    }

    /// [`Self::trajectory`] including what a run in progress has folded but
    /// not yet handed to the state: its closed requests and compactions.
    pub(super) fn trajectory_with(
        &self,
        running_requests: &[crate::meter::Metered],
        running_marks: &[crate::state::CompactionMark],
    ) -> atif::Trajectory {
        let mut agent = sven_session_store::default_agent_profile();
        agent.model_name.clone_from(&self.state.model);
        if !self.state.tools.is_empty() {
            agent.tool_definitions = Some(self.state.tools.clone());
        }
        let mut trajectory = atif::Trajectory::new(sven_session_store::ATIF_SCHEMA_VERSION, agent);
        // The record holds every message in order, with each compaction marked
        // where it fired. A state from before the record, or a run whose
        // events were lost, has only the history, which holds what survived
        // each compaction: the markers of those lead it.
        let record = if self.state.record.is_empty() {
            &self.state.history
        } else {
            &self.state.record
        };
        let compactions: Vec<(usize, sven_vocab::CompactionRecord)> = self
            .state
            .compactions
            .iter()
            .map(|c| (0, c.clone()))
            .chain(
                self.state
                    .compaction_marks
                    .iter()
                    .chain(running_marks)
                    .map(|m| (m.at, m.compaction.clone())),
            )
            .collect();
        let requests: Vec<(usize, sven_vocab::RequestRecord)> = self
            .state
            .requests
            .iter()
            .chain(running_requests)
            .filter_map(|m| Some((m.after?, m.request.clone())))
            .collect();
        trajectory.steps = sven_session_store::record_steps::conversation_steps(
            &sven_session_store::record_steps::ConversationFacts {
                messages: record,
                compactions: &compactions,
                requests: &requests,
                gate_decisions: &self.state.gate_decisions,
                memory_retrievals: &self.state.memory_retrievals,
            },
            &|message| self.state.as_sent(message),
        );
        trajectory.final_metrics = sven_session_store::step_request::totals(
            self.state
                .requests
                .iter()
                .chain(running_requests)
                .map(|metered| &metered.request),
        );
        trajectory
    }
}
