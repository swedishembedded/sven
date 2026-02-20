use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Command;
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

/// Maximum bytes captured from a command's stdout/stderr.
const OUTPUT_LIMIT: usize = 100_000;

/// Built-in tool that runs a shell command.
pub struct ShellTool {
    pub timeout_secs: u64,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str { "shell" }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout + stderr.  \
         Prefer non-interactive commands. Avoid commands that require a TTY."
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

        debug!(cmd = %command, "executing shell tool");

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
                    ToolOutput::err(
                        &call.id,
                        format!("[exit {code}]\n{content}"),
                    )
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

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { id: id.into(), name: "shell".into(), args }
    }

    // ── Successful execution ──────────────────────────────────────────────────

    #[tokio::test]
    async fn executes_echo_and_returns_stdout() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({"command": "echo hello"}))).await;
        assert!(!out.is_error);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn stdout_and_stderr_both_captured() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({
            "command": "echo out && echo err >&2"
        }))).await;
        assert!(out.content.contains("out"));
        assert!(out.content.contains("err"));
    }

    #[tokio::test]
    async fn workdir_changes_cwd() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({
            "command": "pwd",
            "workdir": "/tmp"
        }))).await;
        assert!(!out.is_error);
        assert!(out.content.trim().ends_with("tmp") || out.content.contains("/tmp"));
    }

    // ── Failure cases ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn non_zero_exit_code_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({"command": "exit 1"}))).await;
        assert!(out.is_error, "non-zero exit should set is_error");
        assert!(out.content.contains("[exit 1]"));
    }

    #[tokio::test]
    async fn missing_command_argument_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let t = ShellTool { timeout_secs: 1 };
        let out = t.execute(&call("1", json!({
            "command": "sleep 60",
            "timeout_secs": 1
        }))).await;
        assert!(out.is_error);
        assert!(out.content.contains("timeout"));
    }

    // ── Output truncation ─────────────────────────────────────────────────────

    #[test]
    fn truncate_does_nothing_within_limit() {
        let s = "hello";
        assert_eq!(truncate(s, 100), "hello");
    }

    #[test]
    fn truncate_shortens_and_appends_note() {
        let s = "abcdefghij"; // 10 chars
        let result = truncate(s, 5);
        assert!(result.starts_with("abcde"));
        assert!(result.contains("truncated"));
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_has_required_command_field() {
        let t = ShellTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("command")));
    }
}
