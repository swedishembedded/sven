// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! What the [`crate::McpManager`] tells a frontend, and what to tell the
//! person about it.

/// Events emitted by the manager to signal state changes.
#[derive(Debug)]
pub enum McpEvent {
    /// A server successfully connected.
    ServerConnected(String),
    /// A server disconnected or was disabled.
    ServerDisconnected(String),
    /// A server connection failed.
    ServerFailed { name: String, error: String },
    /// The set of tools/prompts changed; re-register with the tool registry.
    ToolsChanged,
    /// An HTTP server needs a sign-in that this session will not start by
    /// itself (a headless run, or the server refused the previous one).
    AuthRequired { server: String, auth_url: String },
    /// A sign-in started; its page follows as [`McpEvent::SignInLink`].
    AuthStarted { server: String },
    /// The page to sign in to `server` on, for a frontend to show beside the
    /// browser sven tried to open (a remote shell has none).
    SignInLink { server: String, url: String },
}

/// How much a [`McpEvent::notice`] matters to the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Success,
    Warning,
    Error,
}

impl McpEvent {
    /// What to tell the person about this event, the same in every frontend
    /// (a TUI toast, a headless run's stderr); `None` for bookkeeping events.
    #[must_use]
    pub fn notice(&self) -> Option<(NoticeLevel, String)> {
        match self {
            Self::AuthStarted { server } => Some((
                NoticeLevel::Info,
                format!("Signing in to MCP server '{server}' in the browser..."),
            )),
            Self::SignInLink { server, url } => {
                Some((NoticeLevel::Info, crate::sign_in_link(server, url)))
            }
            Self::AuthRequired { server, .. } => Some((
                NoticeLevel::Warning,
                format!(
                    "MCP server '{server}' needs a sign-in: `/mcp auth {server}` in the TUI, \
                     or `sven mcp auth {server}`"
                ),
            )),
            Self::ServerConnected(server) => Some((
                NoticeLevel::Success,
                format!("MCP server '{server}' connected"),
            )),
            Self::ServerFailed { name, error } => Some((
                NoticeLevel::Error,
                format!(
                    "MCP server '{name}': {}",
                    error.chars().take(300).collect::<String>()
                ),
            )),
            Self::ServerDisconnected(_) | Self::ToolsChanged => None,
        }
    }
}
