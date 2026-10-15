// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent reduced to the bytes needed to bring it back.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sven_hsm::Snapshot;
use sven_model::{Message, MessageContent};
use sven_vocab::CompactionRecord;

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
    /// The model's raw argument text, by call id, for tool calls whose
    /// arguments were not valid JSON (the history holds those calls with
    /// `{}`).
    #[serde(default)]
    pub(crate) unparsed_arguments: BTreeMap<String, String>,
    /// The sequence number the agent's next event will carry. Kept here so the
    /// numbering of an agent's events carries on across a resume.
    #[serde(default)]
    pub(crate) next_event_seq: u64,
    /// The run the agent is parked in, if its last run ended waiting on a
    /// question or an approval. The answer or decision continues that run.
    #[serde(default)]
    pub(crate) open_run: Option<RunId>,
}

impl AgentState {
    /// The format this build writes.
    ///
    /// Version 1 added the format version itself, the event sequence and the
    /// parked run. Version 0 is every state written before: it has none of
    /// those, resumes as an agent that has emitted no events and is parked
    /// nowhere, and is written back as the current version. A state with a
    /// version above this one was written by a newer build and is refused by
    /// [`Engine::resume`](crate::Engine::resume) rather than resumed wrongly.
    pub const CURRENT_VERSION: u32 = 1;

    /// Creates the state of an agent that has not run yet.
    #[must_use]
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            next_event_seq: 0,
            open_run: None,
            mode: mode.into(),
            history: Vec::new(),
            role: None,
            kernel: None,
            model: None,
            tools: Vec::new(),
            compactions: Vec::new(),
            unparsed_arguments: BTreeMap::new(),
        }
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
