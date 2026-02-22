// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gdbmi::raw::GeneralMessage;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::{AgentMode, GdbConfig};

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::state::GdbSessionState;

pub struct GdbCommandTool {
    state: Arc<Mutex<GdbSessionState>>,
    cfg: GdbConfig,
}

impl GdbCommandTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>, cfg: GdbConfig) -> Self {
        Self { state, cfg }
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
                    "description": "GDB command to execute (e.g., 'info registers', 'break main', \
                        'continue', 'step', 'next', 'stepi', 'nexti', 'finish', \
                        'backtrace', 'info threads', 'info locals', 'info args', \
                        'x/10x 0x20000000', 'load', 'monitor reset halt')"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Override the default command timeout in seconds. \
                        Use a higher value for slow operations: 'load' (firmware flash, 60-120s), \
                        'monitor erase' (60s), or 'continue' if the target takes time to respond. \
                        Default: the configured command_timeout_secs (typically 10s)."
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

        let timeout_secs = call.args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.cfg.command_timeout_secs);

        debug!(cmd = %command, timeout_secs, "gdb_command");

        let mut state = self.state.lock().await;

        if !state.has_client() {
            return ToolOutput::err(
                &call.id,
                "No active GDB session. Call gdb_connect first.",
            );
        }

        // Temporarily set the timeout for this command, then restore.
        let gdb = state.client.as_mut().unwrap();
        gdb.set_timeout(Duration::from_secs(timeout_secs));
        let gdb = state.client.as_ref().unwrap();

        // Use raw_console_cmd (waits for ^done/^running/^error) then pop_general
        // for console output.  This is safer than raw_console_cmd_for_output because
        // that function requires exactly N lines; if the command emits fewer lines the
        // gdbmi worker gets stuck with pending_console set and blocks ALL subsequent
        // commands until the process is restarted.  With raw_console_cmd, GDB sends all
        // console output lines BEFORE the result token, so by the time raw_cmd returns
        // the lines are already in pending_general and pop_general retrieves them.
        let result = gdb.raw_console_cmd(&command).await;

        // Restore the default timeout regardless of outcome.
        let default_timeout = Duration::from_secs(self.cfg.command_timeout_secs);
        state.client.as_mut().unwrap().set_timeout(default_timeout);

        match result {
            Ok(_resp) => {
                let gdb = state.client.as_ref().unwrap();
                match gdb.pop_general().await {
                    Ok(msgs) => {
                        let lines: Vec<String> = msgs.iter()
                            .filter_map(|m| match m {
                                GeneralMessage::Console(s) => {
                                    // Strip the trailing \n escape that GDB embeds in MI output
                                    Some(s.trim_end_matches("\\n").to_string())
                                }
                                _ => None,
                            })
                            .collect();
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
            Err(e) => ToolOutput::err(&call.id, format!("GDB command error: {e}")),
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use sven_config::GdbConfig;
    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "gdb_command".into(), args }
    }

    fn make_tool() -> GdbCommandTool {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        GdbCommandTool::new(state, GdbConfig::default())
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = make_tool();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_when_not_connected() {
        let t = make_tool();
        let out = t.execute(&call(json!({"command": "info registers"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn fails_with_missing_command() {
        let t = make_tool();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }
}
