// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
#[cfg(unix)]
use libc;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;
use tracing::debug;

use crate::policy::ApprovalPolicy;
use crate::tool::{OutputCategory, Tool, ToolCall, ToolOutput};

/// Hard byte ceiling for combined stdout + stderr returned to the model.
/// 20 KB ≈ 5,000 tokens — keeps output well within a 40 K-token context window.
const OUTPUT_LIMIT_BYTES: usize = 20_000;

/// Number of lines to keep from the head of oversized output.
const HEAD_LINES: usize = 100;

/// Number of lines to keep from the tail of oversized output.
/// Errors and summaries almost always appear at the end of build/test output,
/// so preserving the tail is at least as important as preserving the head.
const TAIL_LINES: usize = 100;

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
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command and return stdout + stderr.\n\
         'command' parameter is required and can be any shell command\n\
         Output is capped at ~20 KB; when larger, the first 100 and last 100 lines are\n\
         preserved with an omission marker in the middle — errors at the end are never lost.\n\
         Prefer non-interactive commands. Avoid commands that require a TTY.\n\
         IMPORTANT: do NOT use shell for file operations:\n\
         - Read files  → use read_file  (not cat / head / tail)\n\
         - Search text → use grep tool  (not grep / rg / ack)\n\
         - Find files  → use glob tool  (not find / ls -R)\n\
         - Edit files  → use edit_file  (not sed / awk / patch)\n\
         For large outputs (builds, test runs), pipe through `tail -200` or\n\
         `grep -E 'error:|warning:' 2>&1` to keep only what matters."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "shell_command": {
                    "type": "string",
                    "description": "The complete bash one liner shell command to execute."
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
            "required": ["shell_command", "workdir", "timeout_secs"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::HeadTail
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let command = match call.args.get("shell_command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "Please provide a shell command to execute as 'shell_command' parameter to this tool call. \
                    The shell command can be any bash one liner",
                );
            }
        };
        let workdir = call
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let timeout = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs);

        debug!(cmd = %command, "executing shell tool");

        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg(&command);
        // Isolate the subprocess from the TUI's terminal.
        //
        // `stdin(Stdio::null())` prevents the subprocess (and any programs it
        // spawns) from accessing the controlling terminal via fd 0.  Most
        // terminal-manipulation code calls `isatty(0)` first; with stdin
        // pointing at /dev/null that returns false and the code is skipped.
        // This is the primary defence against terminal-mode corruption (raw
        // mode being disabled, mouse-reporting strings appearing as text, etc.)
        //
        // `kill_on_drop(true)` ensures that when the timeout fires and the
        // tokio future is dropped, tokio sends SIGKILL to the child before
        // releasing the process handle, preventing zombie processes from
        // continuing to run and potentially interacting with the terminal.
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        // setsid() in pre_exec creates a new session for the child, detaching
        // it from the controlling terminal.  Without this, a subprocess can
        // open /dev/tty directly (bypassing our stdin/stdout/stderr redirects)
        // and send escape sequences (e.g. DisableMouseCapture) that corrupt the
        // TUI state.  With setsid() the child has no controlling terminal, so
        // open("/dev/tty") fails with ENXIO.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        if let Some(wd) = &workdir {
            cmd.current_dir(wd);
        }

        let result =
            tokio::time::timeout(std::time::Duration::from_secs(timeout), cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let mut content = String::new();
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                if !stdout.is_empty() {
                    content.push_str(&head_tail_truncate(&stdout));
                }
                if !stderr.is_empty() {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    content.push_str("[stderr]\n");
                    content.push_str(&head_tail_truncate(&stderr));
                }
                if content.is_empty() {
                    content = format!("[exit {}]", output.status.code().unwrap_or(-1));
                }

                let code = output.status.code().unwrap_or(-1);
                if code == 0 {
                    ToolOutput::ok(&call.id, content)
                } else if code == 1 {
                    // Exit code 1 is the Unix convention for "no matches" (grep/rg),
                    // "condition false" (test/[), and similar non-fatal empty results.
                    // Flagging it as is_error inflates the consecutive-error counter and
                    // confuses the model into believing the command itself failed.
                    // Include the code in the output for transparency.
                    let out = if content.is_empty() {
                        "[exit 1]".to_string()
                    } else {
                        format!("[exit 1]\n{content}")
                    };
                    ToolOutput::ok(&call.id, out)
                } else {
                    ToolOutput::err(&call.id, format!("[exit {code}]\n{content}"))
                }
            }
            Ok(Err(e)) => ToolOutput::err(&call.id, format!("spawn error: {e}")),
            Err(_) => ToolOutput::err(&call.id, format!("timeout after {timeout}s")),
        }
    }
}

/// Truncate `s` to fit within `OUTPUT_LIMIT_BYTES`.
///
/// When truncation is needed the first `HEAD_LINES` and last `TAIL_LINES` are
/// kept verbatim, with an omission marker in the middle showing how many lines
/// and bytes were dropped.  This ensures the model always sees both the
/// beginning of the output (command headers, progress start) and the end
/// (errors, summaries, exit messages) even for very long builds or test runs.
pub(crate) fn head_tail_truncate(s: &str) -> String {
    if s.len() <= OUTPUT_LIMIT_BYTES {
        return s.to_string();
    }

    let lines: Vec<&str> = s.lines().collect();
    let total = lines.len();

    if total <= HEAD_LINES + TAIL_LINES {
        // Enough lines to show everything but byte budget exceeded (very long lines).
        // Fall back to a simple byte-level truncation with a tail window.
        let tail_start = s.len().saturating_sub(OUTPUT_LIMIT_BYTES / 2);
        // Align to a line boundary
        let tail_str = &s[tail_start..];
        let head_end = OUTPUT_LIMIT_BYTES / 2;
        let head_str = &s[..head_end.min(s.len())];
        let omitted_bytes = s.len() - head_str.len() - tail_str.len();
        return format!(
            "{}\n...[{} bytes omitted]...\n{}",
            head_str, omitted_bytes, tail_str
        );
    }

    let head: Vec<&str> = lines[..HEAD_LINES].to_vec();
    let tail: Vec<&str> = lines[total - TAIL_LINES..].to_vec();
    let omitted_lines = total - HEAD_LINES - TAIL_LINES;

    // Approximate omitted bytes for the informational marker.
    let shown_bytes = head.join("\n").len() + tail.join("\n").len();
    let omitted_bytes = s.len().saturating_sub(shown_bytes);

    format!(
        "{}\n...[{} lines / ~{} bytes omitted]...\n{}",
        head.join("\n"),
        omitted_lines,
        omitted_bytes,
        tail.join("\n")
    )
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn call(id: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "shell".into(),
            args,
        }
    }

    // ── Successful execution ──────────────────────────────────────────────────

    #[tokio::test]
    async fn executes_echo_and_returns_stdout() {
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "echo hello"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hello"));
    }

    #[tokio::test]
    async fn stdout_and_stderr_both_captured() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": "echo out && echo err >&2"
                }),
            ))
            .await;
        assert!(out.content.contains("out"));
        assert!(out.content.contains("err"));
    }

    #[tokio::test]
    async fn workdir_changes_cwd() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": "pwd",
                    "workdir": "/tmp"
                }),
            ))
            .await;
        assert!(!out.is_error);
        assert!(out.content.trim().ends_with("tmp") || out.content.contains("/tmp"));
    }

    // ── Failure cases ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn exit_1_is_not_error_but_includes_code() {
        // Exit code 1 is "no matches" for grep/rg and "false" for test — not a hard error.
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "exit 1"})))
            .await;
        assert!(!out.is_error, "exit 1 should not set is_error");
        assert!(out.content.contains("[exit 1]"));
    }

    #[tokio::test]
    async fn exit_2_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call("1", json!({"shell_command": "exit 2"})))
            .await;
        assert!(out.is_error, "exit code >= 2 should set is_error");
        assert!(out.content.contains("[exit 2]"));
    }

    #[tokio::test]
    async fn missing_command_argument_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call("1", json!({}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("shell_command"));
    }

    #[tokio::test]
    async fn timeout_returns_error() {
        let t = ShellTool { timeout_secs: 1 };
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": "sleep 60",
                    "timeout_secs": 1
                }),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("timeout"));
    }

    // ── Head+tail truncation ──────────────────────────────────────────────────

    #[test]
    fn short_output_passes_through_unchanged() {
        let s = "hello\nworld\n";
        assert_eq!(head_tail_truncate(s), s);
    }

    #[test]
    fn large_output_is_truncated_with_omission_marker() {
        // 1000 lines × 30 bytes ≈ 30 KB > OUTPUT_LIMIT_BYTES (20 KB)
        let line = "x".repeat(29);
        let content: String = (0..1000).map(|i| format!("line{i}: {line}\n")).collect();
        let result = head_tail_truncate(&content);
        assert!(
            result.contains("omitted"),
            "should contain omission marker: {result}"
        );
        assert!(result.len() < content.len(), "result should be shorter");
    }

    #[test]
    fn head_and_tail_are_both_preserved() {
        // Build output where first line is "BUILD START" and last is "BUILD ERROR"
        let mut lines: Vec<String> = vec!["BUILD START".to_string()];
        for i in 0..800 {
            lines.push(format!(
                "middle line {i} padding padding padding padding padding"
            ));
        }
        lines.push("BUILD ERROR".to_string());
        let content = lines.join("\n");

        let result = head_tail_truncate(&content);
        assert!(result.contains("BUILD START"), "head should be preserved");
        assert!(result.contains("BUILD ERROR"), "tail should be preserved");
        assert!(result.contains("omitted"), "should have omission marker");
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn schema_has_required_command_field() {
        let t = ShellTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("shell_command")));
    }
}
