// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Compound `gdb` tool that consolidates all 7 GDB tools into a single
//! action-dispatched interface, reducing the model's tool-selection surface.
//!
//! Using one tool instead of seven keeps the tools array short (fewer tokens
//! cached per turn) and makes the GDB workflow self-documenting in one place.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use sven_config::{AgentMode, GdbConfig};

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::{
    command::GdbCommandTool, connect::GdbConnectTool, interrupt::GdbInterruptTool,
    start_server::GdbStartServerTool, state::GdbSessionState, status::GdbStatusTool,
    stop::GdbStopTool, wait_stopped::GdbWaitStoppedTool,
};

/// Compound GDB tool — all GDB actions in one tool definition.
///
/// Each action delegates to the original single-action tool so all existing
/// logic, error messages, and tests remain valid.
pub struct GdbTool {
    start_server: GdbStartServerTool,
    connect: GdbConnectTool,
    command: GdbCommandTool,
    interrupt: GdbInterruptTool,
    wait_stopped: GdbWaitStoppedTool,
    status: GdbStatusTool,
    stop: GdbStopTool,
}

impl GdbTool {
    pub fn new(state: Arc<Mutex<GdbSessionState>>, cfg: GdbConfig) -> Self {
        Self {
            start_server: GdbStartServerTool::new(state.clone(), cfg.clone()),
            connect: GdbConnectTool::new(state.clone(), cfg.clone()),
            command: GdbCommandTool::new(state.clone(), cfg.clone()),
            interrupt: GdbInterruptTool::new(state.clone()),
            wait_stopped: GdbWaitStoppedTool::new(state.clone()),
            status: GdbStatusTool::new(state.clone()),
            stop: GdbStopTool::new(state),
        }
    }
}

#[async_trait]
impl Tool for GdbTool {
    fn name(&self) -> &str {
        "gdb"
    }

    fn description(&self) -> &str {
        "GDB debugging session control for embedded targets.\n\
         action: start_server | connect | command | interrupt | wait_stopped | status | stop\n\n\
         Workflow: start_server → connect → command (loop) → stop\n\
         - start_server: launch JLinkGDBServer/OpenOCD/pyocd in the background\n\
         - connect: spawn gdb-multiarch, load symbols, connect to server\n\
         - command: run any GDB command (break, continue, info registers, backtrace, …)\n\
         - interrupt: send SIGINT to halt a running target (Ctrl+C equivalent)\n\
         - wait_stopped: block until target halts after continue/step\n\
         - status: check server/client/target state without interrupting\n\
         - stop: disconnect gdb-multiarch and kill the server; always call when done"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start_server", "connect", "command", "interrupt", "wait_stopped", "status", "stop"],
                    "description": "Which GDB operation to perform"
                },
                "command": {
                    "type": "string",
                    "description": "[action=command] GDB command to execute, e.g. 'info registers', 'break main', 'continue', 'backtrace'"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "[action=command|interrupt|wait_stopped] Override default timeout in seconds"
                },
                "server_command": {
                    "type": "string",
                    "description": "[action=start_server] Full server command, e.g. 'JLinkGDBServer -device STM32F4 -if SWD -port 2331'. Auto-discovered if omitted."
                },
                "force": {
                    "type": "boolean",
                    "description": "[action=start_server] Kill any existing process on the target port before starting (default false)"
                },
                "host": {
                    "type": "string",
                    "description": "[action=connect] GDB server host (default: localhost)"
                },
                "port": {
                    "type": "integer",
                    "description": "[action=connect] GDB server port; inferred from start_server if omitted"
                },
                "executable": {
                    "type": "string",
                    "description": "[action=connect] Path to ELF binary for debug symbol loading"
                },
                "gdb_path": {
                    "type": "string",
                    "description": "[action=connect] GDB executable path (default: gdb-multiarch)"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn output_category(&self) -> OutputCategory {
        OutputCategory::HeadTail
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let action = match call.args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'action'"),
        };

        match action.as_str() {
            "start_server" => {
                // Remap 'server_command' → 'command' for the delegate tool.
                let mut args = call.args.clone();
                if let Some(sc) = args.get("server_command").cloned() {
                    args["command"] = sc;
                }
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("server_command");
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_start_server".into(),
                    args,
                };
                self.start_server.execute(&delegate_call).await
            }
            "connect" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_connect".into(),
                    args,
                };
                self.connect.execute(&delegate_call).await
            }
            "command" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_command".into(),
                    args,
                };
                self.command.execute(&delegate_call).await
            }
            "interrupt" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_interrupt".into(),
                    args,
                };
                self.interrupt.execute(&delegate_call).await
            }
            "wait_stopped" => {
                let mut args = call.args.clone();
                if let Some(obj) = args.as_object_mut() {
                    obj.remove("action");
                }
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_wait_stopped".into(),
                    args,
                };
                self.wait_stopped.execute(&delegate_call).await
            }
            "status" => {
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_status".into(),
                    args: json!({}),
                };
                self.status.execute(&delegate_call).await
            }
            "stop" => {
                let delegate_call = ToolCall {
                    id: call.id.clone(),
                    name: "gdb_stop".into(),
                    args: json!({}),
                };
                self.stop.execute(&delegate_call).await
            }
            other => ToolOutput::err(
                &call.id,
                format!(
                    "unknown action '{}'. Valid actions: start_server, connect, command, interrupt, wait_stopped, status, stop",
                    other
                ),
            ),
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sven_config::GdbConfig;

    use super::*;
    use crate::tool::ToolCall;

    fn make_tool() -> GdbTool {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        GdbTool::new(state, GdbConfig::default())
    }

    fn call(args: Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "gdb".into(),
            args,
        }
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = make_tool();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }

    #[test]
    fn name_is_gdb() {
        let t = make_tool();
        assert_eq!(t.name(), "gdb");
    }

    #[tokio::test]
    async fn missing_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing required parameter 'action'"));
    }

    #[tokio::test]
    async fn unknown_action_is_error() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "fly"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn status_with_no_session() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "status"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("not connected") || out.content.contains("not started"));
    }

    #[tokio::test]
    async fn stop_with_no_session() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "stop"}))).await;
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn command_fails_when_not_connected() {
        let t = make_tool();
        let out = t
            .execute(&call(
                json!({"action": "command", "command": "info registers"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn interrupt_fails_when_not_connected() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "interrupt"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn wait_stopped_fails_when_not_connected() {
        let t = make_tool();
        let out = t.execute(&call(json!({"action": "wait_stopped"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("No active GDB session"));
    }

    #[tokio::test]
    async fn start_server_fails_with_immediate_exit_command() {
        let t = make_tool();
        let out = t
            .execute(&call(
                json!({"action": "start_server", "server_command": "false"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("exited immediately"));
    }
}
