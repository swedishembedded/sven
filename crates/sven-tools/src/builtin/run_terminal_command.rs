use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::debug;

use sven_config::AgentMode;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

const OUTPUT_LIMIT: usize = 100_000;

pub struct RunTerminalCommandTool {
    pub timeout_secs: u64,
}

impl Default for RunTerminalCommandTool {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[async_trait]
impl Tool for RunTerminalCommandTool {
    fn name(&self) -> &str { "run_terminal_command" }

    fn description(&self) -> &str {
        "Executes a given command in a shell session. \
         Use for terminal operations like git, build tools, and system commands. \
         DO NOT use for file operations (reading, writing, editing, searching, finding files) — \
         use the specialized tools for those instead. \
         Always quote file paths that contain spaces. \
         Avoid commands that require a TTY or interactive input."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (optional, defaults to cwd)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Execution timeout in seconds (optional)"
                }
            },
            "required": ["command"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'command' argument"),
        };
        let workdir = call.args.get("workdir").and_then(|v| v.as_str()).map(str::to_string);
        let timeout = call.args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);

        debug!(cmd = %command, "run_terminal_command tool");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        if let Some(wd) = &workdir {
            cmd.current_dir(wd);
        }

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            cmd.output(),
        ).await;

        match result {
            Ok(Ok(output)) => {
                let mut content = String::new();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !stdout.is_empty() {
                    content.push_str(&truncate(&stdout, OUTPUT_LIMIT));
                }
                if !stderr.is_empty() {
                    if !content.is_empty() { content.push('\n'); }
                    content.push_str("[stderr]\n");
                    content.push_str(&truncate(&stderr, OUTPUT_LIMIT));
                }
                if content.is_empty() {
                    content = format!("[exit {}]", output.status.code().unwrap_or(-1));
                }

                if output.status.success() {
                    ToolOutput::ok(&call.id, content)
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    ToolOutput::err(&call.id, format!("[exit {code}]\n{content}"))
                }
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("spawn error: {e}")),
            Err(_) => ToolOutput::err(&call.id, format!("timeout after {timeout}s")),
        }
    }
}

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        format!("{}...[truncated {} bytes]", &s[..limit], s.len() - limit)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: "run_terminal_command".into(), args }
    }

    #[tokio::test]
    async fn executes_echo_and_returns_stdout() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "echo hello"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn captures_stderr() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "echo err >&2"}))).await;
        assert!(out.content.contains("err"));
    }

    #[tokio::test]
    async fn non_zero_exit_is_error() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({"command": "exit 1"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("[exit 1]"));
    }

    #[tokio::test]
    async fn missing_command_is_error() {
        let t = RunTerminalCommandTool::default();
        let out = t.execute(&call(json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let t = RunTerminalCommandTool { timeout_secs: 1 };
        let out = t.execute(&call(json!({"command": "sleep 60", "timeout_secs": 1}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("timeout"));
    }

    #[test]
    fn only_available_in_agent_mode() {
        let t = RunTerminalCommandTool::default();
        assert_eq!(t.modes(), &[AgentMode::Agent]);
    }
}
