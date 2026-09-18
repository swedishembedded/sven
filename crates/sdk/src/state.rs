// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! An agent reduced to the bytes needed to bring it back.

use serde::{Deserialize, Serialize};
use sven_hsm::Snapshot;
use sven_model::Message;

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
    /// The kernel state the last step ended in.
    ///
    /// `None` for an agent that has not run yet, which resumes from its
    /// machine's initial state.
    pub(crate) kernel: Option<Snapshot>,
}

impl AgentState {
    /// Creates the state of an agent that has not run yet.
    #[must_use]
    pub fn new(mode: impl Into<String>) -> Self {
        Self {
            mode: mode.into(),
            history: Vec::new(),
            kernel: None,
        }
    }

    /// The machine this agent runs.
    #[must_use]
    pub fn mode(&self) -> &str {
        &self.mode
    }

    /// The conversation so far.
    #[must_use]
    pub fn history(&self) -> &[Message] {
        &self.history
    }

    /// The kernel state the last step ended in, if it has run.
    #[must_use]
    pub fn kernel(&self) -> Option<&Snapshot> {
        self.kernel.as_ref()
    }
}
