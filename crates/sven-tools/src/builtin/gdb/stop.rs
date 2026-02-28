// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use libc;
use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

pub struct GdbStopTool {
    state: Arc<Mutex<GdbSessionState>>,
}

impl GdbStopTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for GdbStopTool {
    fn name(&self) -> &str {
        "gdb_stop"
    }

    fn description(&self) -> &str {
        "Stop the active GDB debugging session: disconnect gdb-multiarch and kill the \
         GDB server process (JLinkGDBServer, OpenOCD, etc.). \
         Always call this when done debugging to clean up background processes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        debug!("gdb_stop: tearing down session");

        let mut state = self.state.lock().await;

        if !state.has_server() && !state.has_client() {
            return ToolOutput::ok(&call.id, "No active GDB session to stop.");
        }

        // Send SIGTERM to the GDB process if we know its PID.
        // Avoid raw_console_cmd("quit"): when GDB exits immediately after receiving
        // "quit", its stderr closes with an empty read, triggering a panic in the
        // gdbmi worker's process_stderr() (usize underflow on len()-1 of empty buf).
        // SIGTERM lets GDB exit cleanly without us blocking on its response.
        if let Some(pid) = state.gdb_pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }

        // Brief pause to let GDB drain output before we drop it.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        state.clear().await;

        ToolOutput::ok(&call.id, "GDB session stopped. Server process killed.")
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call() -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb_stop".into(),
            args: json!({}),
        }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStopTool::new(state);
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn succeeds_with_no_session() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStopTool::new(state);
        let out = t.execute(&call()).await;
        assert!(!out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn stops_running_server() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        {
            let mut s = state.lock().await;
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into(), None);
        }
        let t = GdbStopTool::new(state.clone());
        let out = t.execute(&call()).await;
        assert!(!out.is_error);
        assert!(out.content.contains("stopped"));
        // State should be cleared
        let s = state.lock().await;
        assert!(!s.has_server());
    }
}
