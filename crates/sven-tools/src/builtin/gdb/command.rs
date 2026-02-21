use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

pub struct GdbCommandTool {
    state: Arc<Mutex<GdbSessionState>>,
}

impl GdbCommandTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl Tool for GdbCommandTool {
    fn name(&self) -> &str { "gdb_command" }

    fn description(&self) -> &str {
        "Run a GDB command in the active debugging session and return its output. \
         Examples: 'continue', 'break main', 'info registers', 'x/10x 0x20000000', \
         'backtrace', 'load', 'monitor reset halt'. \
         Requires gdb_connect to have been called first."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "GDB command to execute (e.g., 'info registers', 'break main')"
                },
                "capture_lines": {
                    "type": "integer",
                    "description": "Maximum number of console output lines to capture (default: 40)"
                }
            },
            "required": ["command"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'command' argument"),
        };
        let capture_lines = call.args
            .get("capture_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(40) as usize;

        debug!(cmd = %command, "gdb_command");

        let state = self.state.lock().await;

        if !state.has_client() {
            return ToolOutput::err(
                &call.id,
                "No active GDB session. Call gdb_connect first.",
            );
        }

        let gdb = state.client.as_ref().unwrap();

        match gdb.raw_console_cmd_for_output(&command, capture_lines).await {
            Ok((_resp, lines)) => {
                let output = lines.join("\n");
                if output.is_empty() {
                    ToolOutput::ok(&call.id, format!("[command '{command}' produced no output]"))
                } else {
                    ToolOutput::ok(&call.id, output)
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("GDB command error: {e}")),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "gdb_command".into(), args }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbCommandTool::new(state);
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_when_not_connected() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbCommandTool::new(state);
        let out = t.execute(&call(json!({"command": "info registers"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn fails_with_missing_command() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let t = GdbCommandTool::new(state);
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }
}
