// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
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

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Send SIGINT to the GDB process to trigger a hardware halt via the
/// remote debugging protocol.  This is the only reliable way to interrupt
/// a running embedded target because `-exec-interrupt` is not supported
/// while the GDB async executor is active with JLinkGDBServer.
fn send_sigint(pid: u32) {
    // SAFETY: pid is a valid process ID obtained from tokio::process::Child.
    // We only send SIGINT (non-destructive signal) to the GDB process.
    let ret = unsafe { libc::kill(pid as i32, libc::SIGINT) };
    if ret != 0 {
        tracing::warn!(pid, "SIGINT to GDB process failed (errno={})", ret);
    }
}

pub struct GdbInterruptTool {
    state: Arc<Mutex<GdbSessionState>>,
}

impl GdbInterruptTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for GdbInterruptTool {
    fn name(&self) -> &str {
        "gdb_interrupt"
    }

    fn description(&self) -> &str {
        "Interrupt the currently running target (equivalent to pressing Ctrl+C in a GDB prompt). \
         Sends the GDB 'interrupt' command and waits for the target to halt. \
         Use this when the target is running and you need to pause it to inspect state. \
         Requires gdb_connect to have been called first."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "timeout_secs": {
                    "type": "integer",
                    "description": "Seconds to wait for the target to halt after interrupt (default: 5)"
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
            .unwrap_or(5);

        debug!("gdb_interrupt");

        let state = self.state.lock().await;

        if !state.has_client() {
            return ToolOutput::err(&call.id, "No active GDB session. Call gdb_connect first.");
        }

        let gdb = state.client.as_ref().unwrap();
        let pid = state.gdb_pid;

        // Check whether the target is already stopped using a single non-blocking
        // status query.  Using await_stopped() would register an AwaitStatus entry
        // in the gdbmi worker; if it times out the entry stays and causes the
        // worker to fail on all future *stopped notifications.  A simple status()
        // poll is safe and avoids that footgun entirely.
        match gdb.status().await {
            Ok(Status::Stopped(stopped)) => {
                let location = match (&stopped.function, &stopped.file, stopped.line) {
                    (Some(func), Some(file), Some(line)) => format!("{func} ({file}:{line})"),
                    (Some(func), _, _) => func.clone(),
                    _ => format!("PC=0x{:x}", stopped.address.0),
                };
                return ToolOutput::ok(
                    &call.id,
                    format!("Target is already stopped at {location}."),
                );
            }
            Err(e) => {
                return ToolOutput::err(&call.id, format!("Status query failed: {e}"));
            }
            _ => {} // Running or unstarted — proceed with interrupt
        }

        // Send SIGINT to the GDB process.  This is the reliable way to halt an
        // embedded target through JLinkGDBServer: GDB forwards the signal to the
        // target via the GDB remote serial protocol ($03 interrupt packet).
        //
        // Note: -exec-interrupt is NOT used here because JLinkGDBServer returns
        // "Cannot execute this command while the target is running" without a
        // response token, causing raw_cmd to time out indefinitely.
        match pid {
            Some(p) => send_sigint(p),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "Cannot interrupt: GDB process PID is unknown. \
                     Re-connect with gdb_connect.",
                );
            }
        }

        // Poll until stopped or timeout.
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            match gdb.status().await {
                Ok(Status::Stopped(stopped)) => {
                    let location = match (&stopped.function, &stopped.file, stopped.line) {
                        (Some(func), Some(file), Some(line)) => format!("{func} ({file}:{line})"),
                        (Some(func), _, _) => func.clone(),
                        _ => format!("PC=0x{:x}", stopped.address.0),
                    };
                    return ToolOutput::ok(
                        &call.id,
                        format!("Target interrupted and stopped at {location}."),
                    );
                }
                Ok(_) => {
                    if Instant::now() >= deadline {
                        return ToolOutput::err(
                            &call.id,
                            format!("Target did not stop within {timeout_secs}s after interrupt."),
                        );
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("Status poll failed: {e}"));
                }
            }
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb_interrupt".into(),
            args,
        }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbInterruptTool::new(state);
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_when_not_connected() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbInterruptTool::new(state);
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }
}
