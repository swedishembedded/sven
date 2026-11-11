// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! [`AgentMode`], the agent's operating mode as named on the command line and
//! in the event stream.
//!
//! Swedish Embedded AB implements solutions for controlling what an AI agent
//! may do in each phase of work for its clients. If your team needs expertise
//! in agent operating modes and tool-permission profiles, you can procure our
//! services by sending an email to info@swedishembedded.com.

/// The agent's current operating mode, selectable via `--mode`/`/mode`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum AgentMode {
    /// Pure research - read-only tools, no writes
    Research,
    /// Generate a structured plan, no code changes
    Plan,
    /// Full agent with read/write tools
    Agent,
    /// Conversational chat mode (HSM ReactiveAgentMachine in chat mode)
    Chat,
    /// Software development lifecycle mode (HSM SdlcMachine)
    Sdlc,
}

impl AgentMode {
    /// The mode whose [`Display`](std::fmt::Display) form is `name`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        [
            Self::Research,
            Self::Plan,
            Self::Agent,
            Self::Chat,
            Self::Sdlc,
        ]
        .into_iter()
        .find(|mode| mode.to_string() == name)
    }
}

impl std::fmt::Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Research => write!(f, "research"),
            AgentMode::Plan => write!(f, "plan"),
            AgentMode::Agent => write!(f, "agent"),
            AgentMode::Chat => write!(f, "chat"),
            AgentMode::Sdlc => write!(f, "sdlc"),
        }
    }
}
