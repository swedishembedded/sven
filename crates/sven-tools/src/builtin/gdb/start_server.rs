use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::{AgentMode, GdbConfig};

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

use super::discovery::{discover_gdb_server_command, extract_port_from_command};
use super::state::GdbSessionState;

pub struct GdbStartServerTool {
    state: Arc<Mutex<GdbSessionState>>,
    cfg: GdbConfig,
}

impl GdbStartServerTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>, cfg: GdbConfig) -> Self {
        Self { state, cfg }
    }
}

#[async_trait]
impl Tool for GdbStartServerTool {
    fn name(&self) -> &str { "gdb_start_server" }

    fn description(&self) -> &str {
        "Start a GDB debug server in the background (e.g., JLinkGDBServer, OpenOCD, pyocd). \
         If no command is provided, the agent will try to discover the correct command from \
         project files such as .gdbinit, .vscode/launch.json, openocd.cfg, or platformio.ini. \
         Use gdb_connect after this to attach gdb-multiarch to the running server. \
         Only call this once per session; use gdb_stop to shut everything down."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Full GDB server command to run \
                        (e.g., 'JLinkGDBServer -device AT32F435RMT7 -if SWD -speed 4000 -port 2331'). \
                        If omitted the agent discovers the command automatically."
                }
            }
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        {
            let state = self.state.lock().await;
            if state.has_server() {
                return ToolOutput::err(
                    &call.id,
                    "GDB server is already running. Call gdb_stop first if you want to restart.",
                );
            }
        }

        // Determine command
        let command = if let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) {
            cmd.to_string()
        } else {
            match discover_gdb_server_command().await {
                Ok(Some(cmd)) => cmd,
                Ok(None) => return ToolOutput::err(
                    &call.id,
                    "Could not discover a GDB server command from project files. \
                     Please provide the 'command' argument explicitly, e.g.: \
                     JLinkGDBServer -device <DEVICE> -if SWD -speed 4000 -port 2331",
                ),
                Err(e) => return ToolOutput::err(
                    &call.id,
                    format!("Discovery error: {e}"),
                ),
            }
        };

        debug!(cmd = %command, "gdb_start_server: spawning");

        // Spawn the server
        let child = match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(&call.id, format!("Failed to spawn server: {e}")),
        };

        // Determine address
        let port = extract_port_from_command(&command).unwrap_or(2331);
        let addr = format!("localhost:{port}");

        // Brief wait for the server to initialize
        tokio::time::sleep(std::time::Duration::from_millis(
            self.cfg.server_startup_wait_ms,
        )).await;

        // Check if still alive
        let mut state = self.state.lock().await;
        state.set_server(child, addr.clone());

        // Verify the process hasn't immediately exited
        if let Some(server) = &mut state.server {
            match server.try_wait() {
                Ok(Some(status)) => {
                    // Process exited – clear state and report
                    let _ = state.server.take();
                    state.server_addr = None;
                    return ToolOutput::err(
                        &call.id,
                        format!(
                            "GDB server exited immediately ({}). \
                             Check that the server binary is installed and the command is correct.",
                            status
                        ),
                    );
                }
                Ok(None) => {} // still running
                Err(e) => {
                    return ToolOutput::err(&call.id, format!("Could not check server status: {e}"));
                }
            }
        }

        ToolOutput::ok(
            &call.id,
            format!(
                "GDB server started successfully.\n\
                 Command: {command}\n\
                 Listening at: {addr}\n\
                 Call gdb_connect to attach gdb-multiarch."
            ),
        )
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::ToolCall;

    fn call(args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "gdb_start_server".into(), args }
    }

    fn make_tool() -> GdbStartServerTool {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        GdbStartServerTool::new(state, GdbConfig::default())
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = make_tool();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[tokio::test]
    async fn fails_if_command_exits_immediately() {
        let t = make_tool();
        // `false` exits with code 1 immediately
        let out = t.execute(&call(json!({"command": "false"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("exited immediately"));
    }

    #[tokio::test]
    async fn fails_if_already_running() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        // Manually mark server as running
        {
            let mut s = state.lock().await;
            // Start a long-lived dummy process
            let child = tokio::process::Command::new("sleep")
                .arg("60")
                .spawn()
                .unwrap();
            s.set_server(child, "localhost:2331".into());
        }
        let t = GdbStartServerTool::new(state, GdbConfig::default());
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("already running"));
    }
}
