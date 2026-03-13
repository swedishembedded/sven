// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Per-server health tracking and exponential-backoff reconnection.

use std::time::{Duration, Instant};

// ── ServerStatus ──────────────────────────────────────────────────────────────

/// The connection status of an MCP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerStatus {
    /// Never been connected yet (initial state).
    Initializing,
    /// Currently connecting.
    Connecting,
    /// Connected and operational.
    Connected,
    /// Disabled by the user (config `enabled: false`).
    Disabled,
    /// Disconnected, will attempt reconnection.
    Reconnecting { attempts: u32 },
    /// Failed to connect; no further reconnection attempts.
    Failed { error: String },
    /// Requires OAuth authentication before connecting.
    NeedsAuth { auth_url: String },
}

impl ServerStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ServerStatus::Initializing => "initializing",
            ServerStatus::Connecting => "connecting",
            ServerStatus::Connected => "connected",
            ServerStatus::Disabled => "disabled",
            ServerStatus::Reconnecting { .. } => "reconnecting",
            ServerStatus::Failed { .. } => "failed",
            ServerStatus::NeedsAuth { .. } => "needs-auth",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, ServerStatus::Connected)
    }
}

// ── HealthState ───────────────────────────────────────────────────────────────

/// Health tracking for a single MCP server connection.
#[derive(Debug)]
pub struct HealthState {
    pub status: ServerStatus,
    pub consecutive_failures: u32,
    pub last_attempt: Option<Instant>,
    pub tool_count: usize,
    pub prompt_count: usize,
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            status: ServerStatus::Initializing,
            consecutive_failures: 0,
            last_attempt: None,
            tool_count: 0,
            prompt_count: 0,
        }
    }

    /// Record a successful connection.
    pub fn report_ok(&mut self, tool_count: usize, prompt_count: usize) {
        self.status = ServerStatus::Connected;
        self.consecutive_failures = 0;
        self.last_attempt = Some(Instant::now());
        self.tool_count = tool_count;
        self.prompt_count = prompt_count;
    }

    /// Record a connection failure.  Returns the backoff duration to wait
    /// before the next attempt, or `None` if max attempts have been reached.
    pub fn report_error(&mut self, error: String) -> Option<Duration> {
        self.consecutive_failures += 1;
        self.last_attempt = Some(Instant::now());

        const MAX_ATTEMPTS: u32 = 10;
        if self.consecutive_failures >= MAX_ATTEMPTS {
            self.status = ServerStatus::Failed { error };
            return None;
        }

        let delay = backoff_duration(self.consecutive_failures);
        self.status = ServerStatus::Reconnecting {
            attempts: self.consecutive_failures,
        };
        Some(delay)
    }

    /// Whether this server should attempt reconnection.
    pub fn should_reconnect(&self) -> bool {
        matches!(self.status, ServerStatus::Reconnecting { .. })
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

// ── Backoff ───────────────────────────────────────────────────────────────────

/// Exponential backoff: 5s → 10s → 20s → 40s → 80s → 160s → 300s cap.
fn backoff_duration(attempt: u32) -> Duration {
    let exp = 2u64.saturating_pow(attempt.saturating_sub(1));
    let secs = 5u64.saturating_mul(exp).min(300);
    Duration::from_secs(secs)
}

// ── ServerStatusSummary ───────────────────────────────────────────────────────

/// Public summary of a server's current state.
#[derive(Debug, Clone)]
pub struct ServerStatusSummary {
    pub name: String,
    pub status: ServerStatus,
    pub tool_count: usize,
    pub prompt_count: usize,
}

impl ServerStatusSummary {
    pub fn status_icon(&self) -> &'static str {
        match &self.status {
            ServerStatus::Connected => "●",
            ServerStatus::Connecting | ServerStatus::Initializing => "◌",
            ServerStatus::Disabled => "○",
            ServerStatus::Reconnecting { .. } => "↺",
            ServerStatus::Failed { .. } => "✗",
            ServerStatus::NeedsAuth { .. } => "🔐",
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_ok_sets_connected() {
        let mut h = HealthState::new();
        h.report_ok(5, 2);
        assert_eq!(h.status, ServerStatus::Connected);
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.tool_count, 5);
    }

    #[test]
    fn report_error_sets_reconnecting_and_returns_backoff() {
        let mut h = HealthState::new();
        let delay = h.report_error("connection refused".into());
        assert!(delay.is_some());
        assert!(matches!(h.status, ServerStatus::Reconnecting { .. }));
    }

    #[test]
    fn report_error_ten_times_sets_failed() {
        let mut h = HealthState::new();
        let mut last = None;
        for _ in 0..10 {
            last = h.report_error("err".into());
        }
        assert!(last.is_none());
        assert!(matches!(h.status, ServerStatus::Failed { .. }));
    }

    #[test]
    fn backoff_increases() {
        let d1 = backoff_duration(1);
        let d2 = backoff_duration(2);
        let d3 = backoff_duration(3);
        assert!(d1 < d2);
        assert!(d2 < d3);
    }

    #[test]
    fn backoff_caps_at_300s() {
        assert_eq!(backoff_duration(100), Duration::from_secs(300));
    }
}
