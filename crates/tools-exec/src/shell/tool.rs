// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
use async_trait::async_trait;
#[cfg(unix)]
use libc;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::debug;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{OutputCategory, Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;
use sven_tool_registry::ToolPolicy;

/// Hard byte ceiling for combined stdout + stderr returned to the model.
/// 20 KB ≈ 5,000 tokens - keeps output well within a 40 K-token context window.
const OUTPUT_LIMIT_BYTES: usize = 20_000;

/// Number of lines to keep from the head of oversized output.
const HEAD_LINES: usize = 100;

/// Number of lines to keep from the tail of oversized output.
/// Errors and summaries almost always appear at the end of build/test output,
/// so preserving the tail is at least as important as preserving the head.
const TAIL_LINES: usize = 100;

/// Default ceiling on a model-supplied `timeout_secs` (`tools.max_timeout_secs`).
///
/// The argument comes straight from the model, so without a bound a single
/// call could pin a task and a shell essentially forever (`timeout_secs:
/// 4294967295` is ~136 years; a model-issued loop over 1e10 years held one
/// agent attempt idle for 17 minutes). Half an hour covers a legitimate
/// build or test run; a longer one is raised through the setting. The
/// system prompt asks the model to pass 600 s for builds and test suites; a
/// call that names none gets [`ShellTool::timeout_secs`] (30 s by default).
pub const DEFAULT_MAX_TIMEOUT_SECS: u64 = 1800;

/// Built-in tool that runs a shell command.
pub struct ShellTool {
    /// Wall time a command gets when the call names none.
    pub timeout_secs: u64,
    /// Longest wall time a call may ask for; a larger request is cut to it.
    pub max_timeout_secs: u64,
    /// Where a command starts when it names no `workdir` (the root, when
    /// confined), and the root a named `workdir` must stay inside. It does not
    /// confine what the command itself does: a shell can reach any path the
    /// process can.
    pub scope: PathScope,
    /// `tools.deny_patterns`: a command matching one is refused and never
    /// runs, whoever approved it.
    pub policy: Arc<ToolPolicy>,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
            scope: PathScope::default(),
            policy: Arc::default(),
        }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn summary(&self) -> &str {
        "Run a shell command and return its output: build, test, git, file and process work."
    }

    fn description(&self) -> &str {
        "Run a shell command; returns stdout + stderr.\n\
         bash on Unix, cmd.exe on Windows. Non-interactive only: a command needing a\n\
         TTY hangs.\n\
         Output over ~20 KB keeps its first and last 100 lines, so filter big output at\n\
         the source (`tail -200`, `grep -E 'error:|warning:'`)."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            // No `description` for the model to write: a surface shows the
            // command itself, so a paraphrase of it would only spend tokens.
            "properties": {
                "shell_command": {
                    "type": "string",
                    "description": "The command line to run"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory (default: cwd)"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!(
                        "Seconds before the command is killed (default {}, max {})",
                        self.timeout_secs.min(self.max_timeout_secs),
                        self.max_timeout_secs
                    )
                }
            },
            "required": ["shell_command"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }
    fn output_category(&self) -> OutputCategory {
        OutputCategory::HeadTail
    }
    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        // `command` is what most models are trained to send to a shell tool;
        // taking it as well keeps such a model from failing every call.
        let given = call
            .args
            .get("shell_command")
            .or_else(|| call.args.get("command"));
        let command = match given.and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => {
                let keys: Vec<&str> = call
                    .args
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "Please provide a shell command to execute as 'shell_command' parameter to this tool call \
                         (the call gave {keys:?}). The shell command can be any bash one liner"
                    ),
                );
            }
        };
        if self.policy.decide(&command) == ApprovalPolicy::Deny {
            return ToolOutput::err(
                &call.id,
                format!(
                    "refused: the command matches tools.deny_patterns and is never run: {command}"
                ),
            );
        }
        let workdir = match call.args.get("workdir").and_then(|v| v.as_str()) {
            Some(requested) => match self.scope.resolve_for(call, requested) {
                Ok(dir) => Some(dir),
                Err(refused) => return refused,
            },
            None => self.scope.root().map(std::path::Path::to_path_buf),
        };
        let timeout = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs)
            .min(self.max_timeout_secs);

        debug!(cmd = %command, "executing shell tool");

        // Use the platform-appropriate shell interpreter.
        // On Unix/macOS: bash -c <command>
        // On Windows:    cmd /C <command>  (PowerShell is too verbose for tool use)
        #[cfg(unix)]
        let mut cmd = {
            let mut c = Command::new("bash");
            c.arg("-c").arg(&command);
            c
        };
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("cmd");
            c.args(["/C", &command]);
            c
        };

        // Isolate the subprocess from the TUI's terminal.
        //
        // `stdin(Stdio::null())` prevents the subprocess (and any programs it
        // spawns) from accessing the controlling terminal via fd 0.  Most
        // terminal-manipulation code calls `isatty(0)` first; with stdin
        // pointing at /dev/null that returns false and the code is skipped.
        // This is the primary defence against terminal-mode corruption (raw
        // mode being disabled, mouse-reporting strings appearing as text, etc.)
        //
        // `kill_on_drop(true)` kills the shell itself when the child handle is
        // dropped; `kill_group::KillOnDrop` below kills the rest of its
        // process group, so a command's own children do not outlive a
        // timeout and keep touching the terminal or the workspace.
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        // setsid() in pre_exec creates a new session for the child, detaching
        // it from the controlling terminal.  Without this, a subprocess can
        // open /dev/tty directly (bypassing our stdin/stdout/stderr redirects)
        // and send escape sequences (e.g. DisableMouseCapture) that corrupt the
        // TUI state.  With setsid() the child has no controlling terminal, so
        // open("/dev/tty") fails with ENXIO. The new session is also a new
        // process group whose id is the shell's pid, which is what lets a
        // timeout kill the whole group.
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

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ToolOutput::err(&call.id, format!("spawn error: {e}")),
        };
        // Until the command finishes by itself, whatever ends this call early
        // (the timeout, or the future being dropped by a cancel) takes the
        // command's whole process group with it, not only the shell.
        #[cfg(unix)]
        let mut group = kill_group::KillOnDrop::new(child.id());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout),
            child.wait_with_output(),
        )
        .await;
        #[cfg(unix)]
        if result.is_ok() {
            group.disarm();
        }

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
            Err(_) => ToolOutput::err(
                &call.id,
                format!(
                    "timeout after {timeout}s: the command was killed. The limit used was {timeout}s; \
                     the longest a call may ask for is {max}s (tools.max_timeout_secs). Retry with a \
                     larger timeout_secs (up to {max}) if it needs the time, or take a different approach \
                     (smaller steps, a narrower command, running it in the background).",
                    max = self.max_timeout_secs
                ),
            ),
        }
    }
}

#[cfg(unix)]
pub(crate) mod kill_group {
    /// Sends SIGKILL to a process group when dropped, unless disarmed.
    ///
    /// The group is the one `setsid()` made for the command, so its id is the
    /// command's pid. It is only killed while the command has not finished by
    /// itself: a command that completed normally may leave background
    /// processes it deliberately started.
    pub(crate) struct KillOnDrop(Option<libc::pid_t>);

    impl KillOnDrop {
        pub(crate) fn new(pid: Option<u32>) -> Self {
            Self(pid.and_then(|pid| libc::pid_t::try_from(pid).ok()))
        }

        pub(crate) fn disarm(&mut self) {
            self.0 = None;
        }
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            if let Some(pgid) = self.0 {
                // SAFETY: kill(2) with a negative pid signals a process group
                // and touches no memory.
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
            }
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
        // Both offsets are raw byte budgets, so they split multi-byte
        // characters on any non-ASCII output — reached whenever a command
        // emits >20 KB in few lines (minified JS, one-line JSON from curl,
        // `cargo --message-format=json`). `ceil`/`floor` keep the two windows
        // from overlapping.
        let tail_start = s.ceil_char_boundary(s.len().saturating_sub(OUTPUT_LIMIT_BYTES / 2));
        let tail_str = &s[tail_start..];
        let head_end = s.floor_char_boundary((OUTPUT_LIMIT_BYTES / 2).min(s.len()));
        let head_str = &s[..head_end];
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

impl ToolDisplay for ShellTool {
    fn display_name(&self) -> &str {
        "Shell"
    }
    fn category(&self) -> &str {
        "shell"
    }
    fn collapsed_summary(&self, args: &serde_json::Value) -> String {
        sven_tool_api::tool_summary::tool_smart_summary("shell", args)
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

/// `head_tail_truncate` cuts command output at raw byte budgets. Any command
/// emitting >20 KB in few lines takes that path — minified JS, one-line JSON
/// from `curl`, `cargo --message-format=json` — and a multi-byte character
/// straddling either offset panicked the tool task.
#[cfg(test)]
mod truncation_boundary_tests {
    use super::head_tail_truncate;

    #[test]
    fn head_tail_truncate_never_splits_a_character() {
        for unit in ["é", "€", "😀", "a"] {
            // Sweep lengths around the byte budget so both the head cut and the
            // tail cut land inside a character for some input.
            for extra in 0..64 {
                let long_line = unit.repeat(20_000 + extra);
                let _ = head_tail_truncate(&long_line);

                let many_lines = format!("{}\n", unit.repeat(200 + extra)).repeat(300);
                let _ = head_tail_truncate(&many_lines);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

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

    // stderr redirection syntax (>&2) is bash-specific.
    #[cfg(unix)]
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
        let dir = tempfile::tempdir().unwrap();
        let t = ShellTool::default();
        // Use a cross-platform way to print the current directory.
        #[cfg(unix)]
        let cmd = "pwd";
        #[cfg(windows)]
        let cmd = "cd";
        let out = t
            .execute(&call(
                "1",
                json!({
                    "shell_command": cmd,
                    "workdir": dir.path().to_str().unwrap()
                }),
            ))
            .await;
        assert!(!out.is_error);
        // Verify we're in the expected directory (path component should appear).
        let dir_name = dir.path().file_name().unwrap().to_string_lossy();
        assert!(out.content.contains(dir_name.as_ref()), "{}", out.content);
    }

    // ── Failure cases ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn exit_1_is_not_error_but_includes_code() {
        // Exit code 1 is "no matches" for grep/rg and "false" for test - not a hard error.
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

    /// A command matching `tools.deny_patterns` is refused and never runs,
    /// whatever the session's approval mode.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_command_matching_a_deny_pattern_never_runs() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker");
        let t = ShellTool {
            policy: std::sync::Arc::new(sven_tool_registry::ToolPolicy::from_patterns(
                &[],
                &["touch *".to_string()],
            )),
            ..Default::default()
        };
        let out = t
            .execute(&call(
                "1",
                json!({ "shell_command": format!("touch {}", marker.display()) }),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("deny_patterns"), "{}", out.content);
        assert!(!marker.exists(), "the command did not run");
    }

    // `sleep` is Unix-specific; on Windows use `timeout /t N`.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_returns_error() {
        let t = ShellTool {
            timeout_secs: 1,
            ..Default::default()
        };
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_call_cannot_ask_for_more_than_the_configured_ceiling() {
        let t = ShellTool {
            max_timeout_secs: 1,
            ..Default::default()
        };
        let started = std::time::Instant::now();
        let out = t
            .execute(&call(
                "1",
                json!({ "shell_command": "sleep 60", "timeout_secs": 3600 }),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(
            out.content.contains("timeout after 1s")
                && out.content.contains("The limit used was 1s")
                && out.content.contains("longest a call may ask for is 1s")
                && out.content.contains("Retry with a larger timeout_secs"),
            "the result names the limit that applied: {}",
            out.content
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(30));
    }

    #[test]
    fn the_default_ceiling_is_half_an_hour_and_a_bare_call_gets_thirty_seconds() {
        let tool = ShellTool::default();
        assert_eq!(tool.max_timeout_secs, 1800);
        assert_eq!(tool.timeout_secs, 30);
        let schema = tool.parameters_schema().to_string();
        assert!(
            schema.contains("default 30") && schema.contains("max 1800"),
            "{schema}"
        );
    }

    /// Whether process `pid` is gone; an exited but unreaped process counts.
    #[cfg(target_os = "linux")]
    fn process_is_gone(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z')),
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn a_timed_out_command_leaves_none_of_its_processes_behind() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("child.pid");
        // The shell stays alive after starting `sleep`, so the sleep is a
        // grandchild that only a group kill reaches.
        let command = format!("sleep 300 & echo $! > {}; wait", pid_file.display());
        let out = ShellTool::default()
            .execute(&call(
                "1",
                json!({ "shell_command": command, "timeout_secs": 1 }),
            ))
            .await;
        assert!(
            out.is_error && out.content.contains("timeout"),
            "{}",
            out.content
        );

        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        for _ in 0..50 {
            if process_is_gone(pid) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("process {pid} outlived the timed-out command");
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
    fn schema_asks_for_nothing_but_the_command_and_how_to_run_it() {
        let tool = ShellTool::default();
        let schema = tool.parameters_schema();
        let properties = schema["properties"].as_object().unwrap();
        assert!(
            !properties.contains_key("description"),
            "the model is asked to describe its own call"
        );
    }

    #[test]
    fn schema_has_required_command_field() {
        let t = ShellTool::default();
        let schema = t.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("shell_command")));
    }
}

// ─── Adversarial tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod adversarial_tests {
    use serde_json::json;

    use super::*;
    use sven_tool_api::tool::{Tool, ToolCall};

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "adv".into(),
            name: "shell".into(),
            args,
        }
    }

    #[tokio::test]
    async fn the_common_command_argument_name_is_accepted() {
        // Most models are trained on a shell tool whose argument is `command`;
        // a call that names it so runs instead of failing turn after turn.
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"command": "echo hello"}))).await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hello"), "{}", out.content);
    }

    #[tokio::test]
    async fn null_command_value_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": null}))).await;
        assert!(
            out.is_error,
            "null command should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn integer_command_value_is_error() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": 42}))).await;
        assert!(
            out.is_error,
            "integer command should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn empty_command_does_not_crash() {
        let t = ShellTool::default();
        let out = t.execute(&call(json!({"shell_command": ""}))).await;
        // Must return without panicking; is_error is acceptable
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn very_long_command_string_does_not_crash() {
        let t = ShellTool {
            timeout_secs: 5,
            ..Default::default()
        };
        let long_cmd = format!("echo {}", "A".repeat(1_000_000));
        let out = t.execute(&call(json!({"shell_command": long_cmd}))).await;
        // Must complete without panic; output may be truncated
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn command_with_shell_metacharacters_does_not_crash() {
        let t = ShellTool {
            timeout_secs: 5,
            ..Default::default()
        };
        // The shell_command runs under sh -c; these special chars must not cause
        // a panic in the Rust layer even if sh reports an error.
        let out = t
            .execute(&call(
                json!({"shell_command": "echo 'single;quotes && ok'"}),
            ))
            .await;
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn command_producing_large_output_is_truncated_not_oom() {
        let t = ShellTool {
            timeout_secs: 10,
            ..Default::default()
        };
        // Generate ~500 KB of output; the truncation logic must keep memory bounded.
        let out = t
            .execute(&call(json!({"shell_command": "yes | head -c 512000"})))
            .await;
        assert!(
            out.content.len() <= OUTPUT_LIMIT_BYTES * 4,
            "output grew beyond expected bound: {} bytes",
            out.content.len()
        );
    }

    #[tokio::test]
    async fn workdir_pointing_to_nonexistent_path_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "pwd",
                "workdir": "sven_adversarial_nonexistent_dir_xyz"
            })))
            .await;
        assert!(
            out.is_error,
            "nonexistent workdir should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn extra_unknown_fields_are_ignored() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "echo ok",
                "totally_unknown_field": "value",
                "another_unknown": 999
            })))
            .await;
        assert!(
            !out.is_error,
            "unknown extra fields should not cause failure: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn unicode_in_command_does_not_crash() {
        let t = ShellTool::default();
        // RTL override, zero-width joiner, multi-byte sequences
        let out = t
            .execute(&call(
                json!({"shell_command": "echo '日本語 \u{202E}RTL\u{200D}ZWJ'"}),
            ))
            .await;
        let _ = out.is_error;
    }

    #[tokio::test]
    async fn newline_in_workdir_is_error() {
        let t = ShellTool::default();
        let out = t
            .execute(&call(json!({
                "shell_command": "pwd",
                "workdir": "line1\nline2"
            })))
            .await;
        // Paths containing newlines are not valid directory names on Linux.
        assert!(
            out.is_error,
            "newline in workdir should be an error: {}",
            out.content
        );
    }
}
