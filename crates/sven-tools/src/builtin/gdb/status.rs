// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::Arc;

use async_trait::async_trait;
use gdbmi::raw::GeneralMessage;
use gdbmi::status::Status;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

pub struct GdbStatusTool {
    state: Arc<Mutex<GdbSessionState>>,
}

impl GdbStatusTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for GdbStatusTool {
    fn name(&self) -> &str {
        "gdb_status"
    }

    fn description(&self) -> &str {
        "Return the current state of the GDB debugging session without interrupting the target. \
         Reports: whether a GDB server is running, whether gdb-multiarch is connected, \
         and whether the target is stopped or running. \
         When stopped, includes current PC, function, file, and line. \
         Use this to orient yourself before sending commands, or to check if the target is \
         still running after a gdb_command('continue')."
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
        debug!("gdb_status");

        let state = self.state.lock().await;

        let server_status = if state.has_server() {
            format!(
                "Server: running ({})",
                state.server_addr.as_deref().unwrap_or("unknown address")
            )
        } else {
            "Server: not started".to_string()
        };

        if !state.has_client() {
            return ToolOutput::ok(
                &call.id,
                format!(
                    "{server_status}\nGDB: not connected\n\
                     Call gdb_start_server then gdb_connect to start a session. \
                     If the server is already running externally, \
                     gdb_start_server will detect it automatically."
                ),
            );
        }

        let gdb = state.client.as_ref().unwrap();

        // Drain any pending general messages first so status is fresh
        let pending_msgs = gdb.pop_general().await.unwrap_or_default();
        let recent_output: Vec<String> = pending_msgs
            .iter()
            .filter_map(|m| match m {
                GeneralMessage::Console(s) => Some(s.trim_end_matches("\\n").to_string()),
                GeneralMessage::Log(s) => Some(format!("[log] {}", s.trim_end_matches("\\n"))),
                GeneralMessage::Target(s) => {
                    Some(format!("[target] {}", s.trim_end_matches("\\n")))
                }
                _ => None,
            })
            .collect();

        // Use a single non-blocking status() query (reads the gdbmi worker's cached state).
        // This is safe and doesn't register any awaiters that could leave stale entries.
        let target_status = match gdb.status().await {
            Ok(Status::Stopped(stopped)) => {
                let reason = stopped
                    .reason
                    .as_ref()
                    .map(|r| format!("{r:?}"))
                    .unwrap_or_else(|| "unknown".to_string());
                let location = match (&stopped.function, &stopped.file, stopped.line) {
                    (Some(func), Some(file), Some(line)) => format!("{func} ({file}:{line})"),
                    (Some(func), _, _) => func.clone(),
                    _ => format!("PC=0x{:x}", stopped.address.0),
                };
                format!(
                    "Target: stopped\nReason: {reason}\nAt: {location}\nPC: 0x{:x}",
                    stopped.address.0
                )
            }
            Ok(Status::Running) => "Target: running\n\
                 → Use gdb_wait_stopped to wait for it to halt, or\n\
                 → Use gdb_interrupt to forcibly pause it."
                .to_string(),
            Ok(Status::Unstarted) => {
                "Target: not started (GDB connected but no target loaded or program not run)"
                    .to_string()
            }
            Ok(Status::Exited(reason)) => {
                format!("Target: exited ({reason:?})")
            }
            Err(e) => {
                format!("Target: unknown (status query failed: {e})")
            }
        };

        let mut parts = vec![server_status, "GDB: connected".to_string(), target_status];
        if !recent_output.is_empty() {
            parts.push(format!("Pending output:\n{}", recent_output.join("\n")));
        }

        ToolOutput::ok(&call.id, parts.join("\n"))
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call() -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb_status".into(),
            args: json!({}),
        }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStatusTool::new(state);
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn reports_no_session() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbStatusTool::new(state);
        let out = t.execute(&call()).await;
        assert!(!out.is_error);
        assert!(out.content.contains("not connected"));
        assert!(out.content.contains("not started"));
    }
}
