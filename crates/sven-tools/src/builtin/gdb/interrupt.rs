use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

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
    fn name(&self) -> &str { "gdb_interrupt" }

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
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let timeout_secs = call.args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(5);

        debug!("gdb_interrupt");

        let state = self.state.lock().await;

        if !state.has_client() {
            return ToolOutput::err(
                &call.id,
                "No active GDB session. Call gdb_connect first.",
            );
        }

        let gdb = state.client.as_ref().unwrap();

        // Check whether the target is already stopped.  If so, return the
        // current stopped state immediately without sending any command.
        // Sending -exec-interrupt (or the CLI "interrupt") to an already-halted
        // target confuses some GDB servers (e.g. JLinkGDBServer) and can cause
        // spurious *running / *stopped notifications that leave the gdbmi worker
        // in an unexpected state, making all subsequent commands time out.
        let timeout = Duration::from_secs(timeout_secs);
        match gdb.await_stopped(Some(Duration::from_millis(200))).await {
            Ok(stopped) => {
                return ToolOutput::ok(
                    &call.id,
                    format!("Target is already stopped.\n{stopped:?}"),
                );
            }
            Err(_) => {
                // Target is not yet stopped; proceed with the interrupt below.
            }
        }

        // Use the GDB/MI command -exec-interrupt rather than the CLI "interrupt"
        // to avoid wrapping it in -interpreter-exec which can produce extra async
        // notifications on remote targets.
        if let Err(e) = gdb.raw_cmd("-exec-interrupt").await {
            return ToolOutput::err(&call.id, format!("Failed to send interrupt: {e}"));
        }

        // Wait for the target to report a stopped status.
        match gdb.await_stopped(Some(timeout)).await {
            Ok(stopped) => ToolOutput::ok(
                &call.id,
                format!("Target interrupted and stopped.\n{stopped:?}"),
            ),
            Err(e) => ToolOutput::err(
                &call.id,
                format!("Target did not stop within {timeout_secs}s: {e}"),
            ),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "gdb_interrupt".into(), args }
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
