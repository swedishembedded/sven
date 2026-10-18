// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent reduced to the bytes needed to bring it back.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sven_hsm::Snapshot;
use sven_model::{Message, MessageContent};
use sven_vocab::CompactionRecord;

use crate::meter::Metered;
use crate::run::{Approval, RunId};

/// A suspended agent: everything needed to resume it, and nothing that cannot
/// outlive the process that made it.
///
/// Holds no connections, channels, executors or registries - those belong to
/// the [`Engine`](crate::Engine) that resumes it, which is why a state
/// suspended by one engine can be resumed by any other engine configured to
/// run the same mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentState {
    /// The format this state was written in; see [`AgentState::CURRENT_VERSION`].
    /// Absent from states written before states were versioned, which read
    /// as version 0.
    #[serde(default)]
    pub(crate) version: u32,
    /// Which machine this agent runs.
    pub(crate) mode: String,
    /// The conversation so far.
    pub(crate) history: Vec<Message>,
    /// The stable role this agent plays, rendered as its system prompt.
    ///
    /// Belongs to the agent rather than to any one call, which is what lets it
    /// sit in the cacheable prefix of every prompt the agent sends.
    #[serde(default)]
    pub(crate) role: Option<String>,
    /// The kernel state the last step ended in.
    ///
    /// `None` for an agent that has not run yet, which resumes from its
    /// machine's initial state.
    pub(crate) kernel: Option<Snapshot>,
    /// The model the last step ran on, for the trajectory.
    #[serde(default)]
    pub(crate) model: Option<String>,
    /// The tools the last step offered the model, as ATIF tool definitions.
    #[serde(default)]
    pub(crate) tools: Vec<serde_json::Value>,
    /// Every compaction that replaced the front of `history`, oldest first.
    ///
    /// The history holds only what survived them, so this is the only trace
    /// that the conversation was once longer.
    #[serde(default)]
    pub(crate) compactions: Vec<CompactionRecord>,
    /// The whole conversation as it happened, append-only: unlike `history`,
    /// which is what the model reads next and shrinks when a compaction
    /// replaces its front, nothing is ever removed from the record. The
    /// trajectory is built from it, because a training or evaluation record
    /// must hold every step the agent took, not the summary that replaced
    /// them. Empty in a state written before the record existed; the first
    /// run then starts it from the history.
    #[serde(default)]
    pub(crate) record: Vec<Message>,
    /// Where in `record` each compaction fired: it replaced the model's view
    /// of the messages before that position.
    #[serde(default)]
    pub(crate) compaction_marks: Vec<CompactionMark>,
    /// The model's raw argument text, by call id, for tool calls whose
    /// arguments were not valid JSON (the history holds those calls with
    /// `{}`).
    #[serde(default)]
    pub(crate) unparsed_arguments: BTreeMap<String, String>,
    /// What the permission gate decided about each call, by the model's call
    /// id, with any argument violations a schema check found. Kept beside the
    /// history because that is where the calls are.
    #[serde(default)]
    pub(crate) gate_decisions: Vec<atif::GateDecision>,
    /// What each memory retrieval the agent's tools made returned, oldest
    /// first.
    #[serde(default)]
    pub(crate) memory_retrievals: Vec<atif::MemoryRetrieval>,
    /// The sequence number the agent's next event will carry. Kept here so the
    /// numbering of an agent's events carries on across a resume.
    #[serde(default)]
    pub(crate) next_event_seq: u64,
    /// The run the agent is parked in, if its last run ended waiting on a
    /// question or an approval. The answer or decision continues that run.
    #[serde(default)]
    pub(crate) open_run: Option<RunId>,
    /// Every model request the agent has made, oldest first.
    #[serde(default)]
    pub(crate) requests: Vec<Metered>,
}

/// A compaction and the position in the record where it fired.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CompactionMark {
    pub(crate) at: usize,
    pub(crate) compaction: CompactionRecord,
}

impl AgentState {
    /// The format this build writes.
    ///
    /// Version 4 added the record, the whole conversation the trajectory is
    /// built from; version 3 added the account of each model request; version 2 added the gate decisions and memory retrievals of the agent's
    /// calls; version 1 added the format version itself, the event sequence and
    /// the parked run. Version 0 is every state written before: it has none of
    /// those, resumes as an agent that has emitted no events and is parked
    /// nowhere, and is written back as the current version. A state with a
    /// version above this one was written by a newer build and is refused by
    /// [`Engine::resume`](crate::Engine::resume) rather than resumed wrongly.
    pub const CURRENT_VERSION: u32 = 4;

    /// Creates the state of an agent that has not run yet.
    #[must_use]
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            next_event_seq: 0,
            open_run: None,
            requests: Vec::new(),
            mode: mode.into(),
            history: Vec::new(),
            role: None,
            kernel: None,
            model: None,
            tools: Vec::new(),
            compactions: Vec::new(),
            record: Vec::new(),
            compaction_marks: Vec::new(),
            unparsed_arguments: BTreeMap::new(),
            gate_decisions: Vec::new(),
            memory_retrievals: Vec::new(),
        }
    }

    /// Keeps the records the session's store holds about the calls its run
    /// made. A decision replaces an earlier one about the same call: a
    /// parked approval is asked in one run and decided in the next.
    pub(crate) fn absorb_call_records(
        &mut self,
        decisions: Vec<atif::GateDecision>,
        retrievals: &[atif::MemoryRetrieval],
    ) {
        for decision in decisions {
            match self
                .gate_decisions
                .iter_mut()
                .find(|known| known.call_id == decision.call_id)
            {
                Some(known) => *known = decision,
                None => self.gate_decisions.push(decision),
            }
        }
        self.memory_retrievals.extend(retrievals.iter().cloned());
    }

    /// Keeps the account of the requests a run made. `lined_up` is whether
    /// the history still holds the run's messages where the run observed them;
    /// when it does not, the requests count towards the totals but are on no
    /// step.
    pub(crate) fn absorb_requests(&mut self, mut requests: Vec<Metered>, lined_up: bool) {
        if !lined_up {
            requests.iter_mut().for_each(|r| r.after = None);
        }
        self.requests.extend(requests);
    }

    /// Takes every request off its step: a compaction replaced the front of
    /// the history, so no earlier position in it means what it did.
    pub(crate) fn unplace_requests(&mut self) {
        self.requests.iter_mut().for_each(|r| r.after = None);
    }

    /// Starts the record from the history for a state that has none (one
    /// written before the record existed, or fresh).
    pub(crate) fn ensure_record(&mut self) {
        if self.record.is_empty() && !self.history.is_empty() {
            self.record.clone_from(&self.history);
        }
    }

    /// Appends `message` to both what the model reads and the record.
    pub(crate) fn push_message(&mut self, message: Message) {
        self.record.push(message.clone());
        self.history.push(message);
    }

    /// `message` as the model sent it: a tool call whose arguments were not
    /// valid JSON carries the raw text again instead of the `{}` the history
    /// holds, so an exported trajectory can keep and flag it.
    pub(crate) fn as_sent(&self, message: &Message) -> Message {
        let mut message = message.clone();
        if let MessageContent::ToolCall {
            tool_call_id,
            function,
        } = &mut message.content
        {
            if let Some(raw) = self.unparsed_arguments.get(tool_call_id) {
                function.arguments.clone_from(raw);
            }
        }
        message
    }

    /// The format this state was written in.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// This state in the format this build writes. Only the version changes:
    /// every field a newer format added has a default that an older state
    /// resumes with.
    pub(crate) fn migrated(mut self) -> Self {
        self.version = Self::CURRENT_VERSION;
        self
    }

    /// The machine this agent runs.
    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    /// The approval this agent is parked on, if its last run ended
    /// [`RunConclusion::Waiting`](crate::RunConclusion::Waiting) on one.
    ///
    /// Read from the state alone, so a host that restarted finds what is
    /// waiting for a decision without running anything.
    #[must_use]
    pub fn pending_approval(&self) -> Option<Approval> {
        let kernel = self.kernel.as_ref()?;
        let pending = kernel.context.pending_approval.as_ref()?;
        Some(Approval::from_pending(
            pending,
            self.open_run?,
            &kernel.context,
        ))
    }

    /// The conversation so far.
    #[must_use]
    /// What this agent exchanged with the model, in a form an application
    /// built on the facade alone can read.
    ///
    /// `history` returns the kernel's own message type, which the facade
    /// deliberately does not publish; this is the same content as a small
    /// read-only view. See [`crate::Turn`].
    pub fn transcript(&self) -> Vec<crate::Turn> {
        crate::transcript::from_history(&self.history)
    }

    /// The raw kernel message history this agent will resume from.
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// The stable role this agent plays, if one was set.
    #[must_use]
    pub fn role(&self) -> Option<&str> {
        self.role.as_deref()
    }

    /// Sets the stable role this agent plays.
    #[must_use]
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role = Some(role.into());
        self
    }

    /// The kernel state the last step ended in, if it has run.
    #[must_use]
    pub fn kernel(&self) -> Option<&Snapshot> {
        self.kernel.as_ref()
    }
}
