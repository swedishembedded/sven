use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

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
    fn name(&self) -> &str { "gdb_stop" }

    fn description(&self) -> &str {
        "Stop the active GDB debugging session: disconnect gdb-multiarch and kill the \
         GDB server process (JLinkGDBServer, OpenOCD, etc.). \
         Always call this when done debugging to clean up background processes."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        debug!("gdb_stop: tearing down session");

        let mut state = self.state.lock().await;

        if !state.has_server() && !state.has_client() {
            return ToolOutput::ok(&call.id, "No active GDB session to stop.");
        }

        // Attempt graceful GDB quit before dropping the client
        if let Some(gdb) = &state.client {
            let _ = gdb.raw_console_cmd("quit").await;
        }

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
        ToolCall { id: "t1".into(), name: "gdb_stop".into(), args: json!({}) }
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
            s.set_server(child, "localhost:2331".into());
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
