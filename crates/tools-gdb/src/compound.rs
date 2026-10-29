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

use crate::GdbConfig;
use sven_vocab::AgentMode;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

use super::{
    command::GdbCommandTool, connect::GdbConnectTool, interrupt::GdbInterruptTool,
    start_server::GdbStartServerTool, state::GdbSessionState, status::GdbStatusTool,
    stop::GdbStopTool, wait_stopped::GdbWaitStoppedTool,
};

/// Compound GDB tool - all GDB actions in one tool definition.
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
    /// `tools.deny_patterns`, applied to every host command the tool runs.
    policy: Arc<sven_tool_registry::ToolPolicy>,
    /// `tools.gdb.allow_host_commands`.
    allow_host_commands: bool,
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
            policy: Arc::default(),
            allow_host_commands: cfg.allow_host_commands,
        }
    }

    /// Refuses a host command - a debug server to start, or a GDB command's
    /// `shell`, `!` or `pipe` passthrough - whose text matches
    /// `tools.deny_patterns`, as the `shell` tool refuses one.
    #[must_use]
    pub fn with_command_policy(mut self, policy: Arc<sven_tool_registry::ToolPolicy>) -> Self {
        self.policy = policy;
        self
    }

    /// What in this call would make the host run a program the model spells
    /// out, if anything: see `tools.gdb.allow_host_commands`.
    fn host_reaching(action: &str, args: &Value) -> Option<String> {
        let given = |key: &str| args.get(key).and_then(Value::as_str);
        match action {
            "start_server" => given("server_command")
                .or_else(|| given("command"))
                .map(|_| "a debug server command".to_string()),
            "connect" => given("gdb_path").map(|_| "a gdb executable path".to_string()),
            "command" => host_reaching_word(given("command")?)
                .map(|word| format!("the GDB command word '{word}'")),
            _ => None,
        }
    }

    /// The host command `call` would run, if it runs one: the server command
    /// `start_server` spawns (spelled `server_command` or `command`), or the
    /// shell side of a GDB `shell`, `!`, `pipe` or `|` command.
    fn host_command(action: &str, args: &Value) -> Option<String> {
        let text = |key: &str| args.get(key).and_then(Value::as_str).map(str::trim);
        match action {
            "start_server" => text("server_command")
                .or_else(|| text("command"))
                .map(str::to_string),
            "command" => gdb_shell_command(text("command")?).map(str::to_string),
            _ => None,
        }
    }
}

/// GDB commands that run host programs or arbitrary code, or can be made
/// to: a command line naming one of them as any word is refused when
/// `tools.gdb.allow_host_commands` is false. Words are matched as GDB
/// matches commands, by unique prefix, so every prefix of a listed word
/// counts (`so` is `source`, `pyt` is `python`). Conservative on purpose:
/// a debugging command with a false match can be spelled another way, an
/// escape that is missed hands the host to whoever drives the session.
const HOST_REACHING_COMMANDS: &[&str] = &[
    "shell",
    "python",
    "python-interactive",
    "pipe",
    "guile",
    "guile-repl",
    "source",
    "compile",
    "eval",
    "define",
    "document",
    "commands",
    "with",
    "exec-wrapper",
    "startup-with-shell",
];

/// The word of `command` that reaches the host, if one does.
fn host_reaching_word(command: &str) -> Option<String> {
    let command = command.trim_start();
    if command.starts_with('!') || command.starts_with('|') {
        return Some(command[..1].to_string());
    }
    command
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '$'))
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .find(|word| {
            let word = word.trim_start_matches('$');
            // `$_shell(...)` runs a program from inside an expression.
            word.contains("shell")
                || (word.len() >= 2
                    && HOST_REACHING_COMMANDS
                        .iter()
                        .any(|reaching| reaching.starts_with(word)))
        })
}

/// The shell command a GDB command line hands to the host shell, if any.
///
/// `shell CMD` and `!CMD` run `CMD`. `pipe GDBCMD | CMD` and its alias
/// `| GDBCMD | CMD` run `CMD`; `pipe -d DELIM GDBCMD DELIM CMD` splits on
/// `DELIM` instead. A pipe without a delimiter repeats the last GDB command
/// into its whole remainder.
fn gdb_shell_command(command: &str) -> Option<&str> {
    let command = command.trim_start();
    if let Some(rest) = command.strip_prefix('!') {
        return Some(rest.trim());
    }
    let (word, rest) = match command.strip_prefix('|') {
        Some(rest) => ("pipe", rest),
        None => command
            .split_once(char::is_whitespace)
            .unwrap_or((command, "")),
    };
    match word {
        "shell" => Some(rest.trim()),
        "pipe" => {
            let rest = rest.trim_start();
            let (delimiter, piped) = match rest.strip_prefix("-d") {
                Some(after) if after.starts_with(char::is_whitespace) => {
                    let after = after.trim_start();
                    after.split_once(char::is_whitespace).unwrap_or((after, ""))
                }
                _ => ("|", rest),
            };
            Some(
                piped
                    .split_once(delimiter)
                    .map_or(piped, |(_, shell)| shell)
                    .trim(),
            )
        }
        _ => None,
    }
}

#[async_trait]
impl Tool for GdbTool {
    fn name(&self) -> &str {
        "gdb"
    }

    fn summary(&self) -> &str {
        "Debug embedded targets with GDB: start a server, connect, run commands, inspect state."
    }

    fn description(&self) -> &str {
        "GDB debugging session control for embedded targets.\n\
         action: start_server | connect | command | interrupt | wait_stopped | status | stop\n\n\
         Workflow: start_server → connect → command (loop) → stop\n\
         - start_server: launch JLinkGDBServer/OpenOCD/pyocd in the background\n\
         - connect: spawn gdb-multiarch, load symbols, connect to server\n\
         - command: run any GDB command (break, continue, info registers, backtrace, ...)\n\
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
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
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

        if !self.allow_host_commands {
            if let Some(what) = Self::host_reaching(&action, &call.args) {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "refused: {what} would run a program on this machine, and \
                         tools.gdb.allow_host_commands is false"
                    ),
                );
            }
        }

        if let Some(host_command) = Self::host_command(&action, &call.args) {
            if self.policy.decide(&host_command) == ApprovalPolicy::Deny {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "refused: the command matches tools.deny_patterns and is never run: \
                         {host_command}"
                    ),
                );
            }
        }

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
    use crate::GdbConfig;
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::ToolCall;

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

    /// A host command the GDB tool would run is refused when its text
    /// matches a deny pattern: a server to start, whichever field spells
    /// it, or the shell side of a `shell`/`!`/`pipe`/`|` passthrough.
    #[tokio::test]
    async fn a_denied_host_command_is_refused() {
        let policy = sven_tool_registry::ToolPolicy::from_patterns(&[], &["rm *".to_string()]);
        let t = make_tool().with_command_policy(Arc::new(policy));
        for args in [
            json!({"action": "command", "command": "shell rm -rf build"}),
            json!({"action": "command", "command": "!rm -rf build"}),
            json!({"action": "command", "command": "pipe rm -rf build"}),
            json!({"action": "command", "command": "|rm -rf build"}),
            json!({"action": "command", "command": "| info registers | rm -rf build"}),
            json!({"action": "command", "command": "pipe -d XX bt XX rm -rf build"}),
            json!({"action": "start_server", "server_command": "rm -rf build"}),
            json!({"action": "start_server", "command": "rm -rf build"}),
        ] {
            let out = t.execute(&call(args.clone())).await;
            assert!(
                out.content.contains("deny_patterns"),
                "{args}: {}",
                out.content
            );
        }
        let out = t
            .execute(&call(
                json!({"action": "command", "command": "info registers"}),
            ))
            .await;
        assert!(!out.content.contains("deny_patterns"));
    }

    /// With `allow_host_commands: false` nothing the model spells reaches
    /// the host: not a GDB escape of any kind, not a server command, not a
    /// gdb path. Ordinary debugging commands still get through to the
    /// session (which then reports there is none).
    #[tokio::test]
    async fn host_commands_are_refused_when_not_allowed() {
        let state = Arc::new(Mutex::new(GdbSessionState::default()));
        let cfg = GdbConfig {
            allow_host_commands: false,
            ..GdbConfig::default()
        };
        let t = GdbTool::new(state, cfg);
        for command in [
            "shell id",
            "  !id",
            "| bt | id",
            "pipe bt | id",
            "python import os",
            "py print(1)",
            "pi",
            "python-interactive",
            "source ~/x.gdb",
            "so x",
            "guile (display 1)",
            "compile code int x;",
            "eval \"shell id\"",
            "print $_shell(\"id\")",
            "define x",
            "commands 1",
            "with confirm off -- shell id",
            "set exec-wrapper env",
            "set startup-with-shell on",
            "-interpreter-exec console \"shell id\"",
        ] {
            let out = t
                .execute(&call(json!({"action": "command", "command": command})))
                .await;
            assert!(
                out.is_error && out.content.contains("allow_host_commands"),
                "{command}: {}",
                out.content
            );
        }
        for args in [
            json!({"action": "start_server", "server_command": "openocd"}),
            json!({"action": "start_server", "command": "openocd"}),
            json!({"action": "connect", "gdb_path": "evil-gdb"}),
        ] {
            let out = t.execute(&call(args.clone())).await;
            assert!(
                out.is_error && out.content.contains("allow_host_commands"),
                "{args}: {}",
                out.content
            );
        }
        for command in [
            "info registers",
            "info sharedlibrary",
            "x/10x 0x20000000",
            "break main",
            "continue",
            "monitor reset halt",
            "print a != b",
            "step",
            "backtrace",
            "load",
        ] {
            let out = t
                .execute(&call(json!({"action": "command", "command": command})))
                .await;
            assert!(
                out.content.contains("No active GDB session"),
                "{command}: {}",
                out.content
            );
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
