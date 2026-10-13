// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent reduced to the bytes needed to bring it back.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sven_hsm::Snapshot;
use sven_model::{Message, MessageContent};
use sven_vocab::CompactionRecord;

/// A suspended agent: everything needed to resume it, and nothing that cannot
/// outlive the process that made it.
///
/// Holds no connections, channels, executors or registries - those belong to
/// the [`Engine`](crate::Engine) that resumes it, which is why a state
/// suspended by one engine can be resumed by any other engine configured to
/// run the same mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentState {
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
}

impl AgentState {
    /// Creates the state of an agent that has not run yet.
    #[must_use]
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
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

    /// The machine this agent runs.
    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
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
