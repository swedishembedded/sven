// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use gdbmi::status::Status;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

/// Poll interval when waiting for the target to stop.
/// Using polling rather than `await_status` avoids leaving stale awaiters in the
/// gdbmi worker's `status_awaiters` list, which would cause the worker to fail on
/// every subsequent `*stopped` notification.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct GdbWaitStoppedTool {
    state: Arc<Mutex<GdbSessionState>>,
}

impl GdbWaitStoppedTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for GdbWaitStoppedTool {
    fn name(&self) -> &str {
        "gdb_wait_stopped"
    }

    fn description(&self) -> &str {
        "Wait for the target to halt and return where it stopped. \
         Call this after gdb_command('continue'), gdb_command('step'), \
         gdb_command('next'), gdb_command('stepi'), gdb_command('nexti'), \
         or gdb_command('finish') to block until execution pauses. \
         Returns the stop reason (breakpoint, watchpoint, signal, etc.), \
         current PC, function name, file, and line number. \
         This is the essential counterpart to 'continue' for breakpoint-driven debugging: \
         set a breakpoint with gdb_command('break <location>'), then call \
         gdb_command('continue') followed by gdb_wait_stopped to land at the breakpoint."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_secs": {
                    "type": "integer",
                    "description": "Seconds to wait for the target to halt (default: 30). \
                        Increase for long-running tests or slow targets."
                }
            },
            "required": ["timeout_secs"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Auto
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::HeadTail
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let timeout_secs = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30);

        debug!(timeout_secs, "gdb_wait_stopped");

        let state = self.state.lock().await;

        if !state.has_client() {
            return ToolOutput::err(&call.id, "No active GDB session. Call gdb_connect first.");
        }

        let gdb = state.client.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        // Poll the cached status rather than registering an async awaiter.
        // Using await_status would leave stale entries in the gdbmi worker's
        // status_awaiters after a timeout, causing it to fail on every subsequent
        // *stopped notification.  Polling is simpler and equally correct.
        loop {
            match gdb.status().await {
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

                    return ToolOutput::ok(
                        &call.id,
                        format!("Target stopped.\nReason: {reason}\nLocation: {location}"),
                    );
                }
                Ok(Status::Exited(reason)) => {
                    return ToolOutput::err(
                        &call.id,
                        format!("Target exited with reason: {reason:?}"),
                    );
                }
                Ok(Status::Running) | Ok(Status::Unstarted) => {
                    if Instant::now() >= deadline {
                        return ToolOutput::err(
                            &call.id,
                            format!(
                                "Target did not stop within {timeout_secs}s.\n\
                                 → Is the target running? (check with gdb_status)\n\
                                 → Did you call gdb_command('continue') or gdb_command('step') first?\n\
                                 → Increase timeout_secs if the target needs longer to reach the breakpoint.\n\
                                 → Use gdb_interrupt to forcibly halt the target."
                            ),
                        );
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("GDB status query failed: {e}"));
                }
            }
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb_wait_stopped".into(),
            args,
        }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbWaitStoppedTool::new(state);
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_when_not_connected() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbWaitStoppedTool::new(state);
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn uses_default_timeout() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbWaitStoppedTool::new(state);
        // No session → error before any waiting happens
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
    }
}
