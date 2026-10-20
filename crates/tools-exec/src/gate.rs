// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The gate command tool: run an argv, report how it ended.
//!
//! This is the process half of a harness `gate` step. It is a tool that no
//! agent mode lists ([`Tool::modes`] is empty) and that the harness runtime
//! never admits to an agent step, so only a workflow's own gates run it. It
//! runs the program directly - there is no shell to interpolate into - in a
//! new session with its working directory confined to the workspace, kills the
//! whole process group when the time is up, and reports the facts: exit
//! status, whether the time ran out, whether the program could be started at
//! all, and the end of each output stream. It does not decide whether that is
//! success; the harness machine does.
//!
//! Swedish Embedded AB implements verifiable command execution for agent
//! workflows for its clients. If your team needs expertise in running build
//! and test commands as trusted checks then you can procure our services by
//! sending an email to info@swedishembedded.com.

use std::process::Stdio;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use sven_harness_spec::{GATE_TOOL, MAX_GATE_TIMEOUT_SECS};
use sven_hsm::ToolCapability;
use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{Tool, ToolCall, ToolOutput};
use sven_tool_api::PathScope;
use sven_vocab::AgentMode;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// How much of the end of each output stream is kept. Test runners put their
/// verdict and failures last, so the tail is what a repair needs.
const STREAM_TAIL_BYTES: usize = 16 * 1024;

/// The most text a gate may be given on standard input.
const MAX_STDIN_BYTES: usize = 32 * 1024 * 1024;

/// How long the output readers get to finish after the program has ended,
/// for a program whose descendants hold the pipes open.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// The last [`STREAM_TAIL_BYTES`] of a stream and whether more came before.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn drain<R: AsyncRead + Unpin>(mut reader: R) -> Tail {
    let mut tail = Tail::default();
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                tail.bytes.extend_from_slice(&chunk[..n]);
                if tail.bytes.len() > 2 * STREAM_TAIL_BYTES {
                    let drop = tail.bytes.len() - STREAM_TAIL_BYTES;
                    tail.bytes.drain(..drop);
                    tail.truncated = true;
                }
            }
        }
    }
    if tail.bytes.len() > STREAM_TAIL_BYTES {
        let drop = tail.bytes.len() - STREAM_TAIL_BYTES;
        tail.bytes.drain(..drop);
        tail.truncated = true;
    }
    tail
}

async fn finish(task: tokio::task::JoinHandle<Tail>) -> Tail {
    match tokio::time::timeout(DRAIN_GRACE, task).await {
        Ok(Ok(tail)) => tail,
        _ => Tail::default(),
    }
}

/// The report of a run, in the shape the harness machine reads.
struct Report {
    exit_code: Option<i32>,
    timed_out: bool,
    error: Option<String>,
    stdout: Tail,
    stderr: Tail,
    elapsed: Duration,
}

impl Report {
    fn failed_to_start(error: String) -> Self {
        Self {
            exit_code: None,
            timed_out: false,
            error: Some(error),
            stdout: Tail::default(),
            stderr: Tail::default(),
            elapsed: Duration::ZERO,
        }
    }

    fn to_json(&self) -> String {
        let text = |tail: &Tail| String::from_utf8_lossy(&tail.bytes).into_owned();
        json!({
            "exit_code": self.exit_code,
            "timed_out": self.timed_out,
            "error": self.error,
            "stdout": text(&self.stdout),
            "stderr": text(&self.stderr),
            "truncated": self.stdout.truncated || self.stderr.truncated,
            "duration_ms": u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX),
        })
        .to_string()
    }
}

/// Runs a harness gate's command inside the workspace.
pub struct GateCommandTool {
    scope: PathScope,
}

impl GateCommandTool {
    /// A gate tool whose commands start in, and may only be pointed at
    /// directories inside, `root`.
    ///
    /// # Errors
    ///
    /// When `root` does not exist or is not a directory.
    pub fn new(root: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        Ok(Self {
            scope: PathScope::confined(root)?,
        })
    }

    async fn run(
        &self,
        argv: &[String],
        dir: &std::path::Path,
        limit: Duration,
        stdin: Option<String>,
    ) -> Report {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // A new session: the program has no controlling terminal and its
        // process group, named by its pid, can be killed as a whole.
        #[cfg(unix)]
        // SAFETY: setsid(2) touches no memory and is async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let started = Instant::now();
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => return Report::failed_to_start(format!("{}: {e}", argv[0])),
        };
        #[cfg(unix)]
        let mut group = crate::shell::kill_group::KillOnDrop::new(child.id());
        let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
            return Report::failed_to_start("the program's output pipes are missing".into());
        };
        // The input is written from a task of its own, so a command that reads
        // slowly, or not at all, cannot stall the wait; a pipe it closed early is
        // the command's business.
        if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = pipe.write_all(text.as_bytes()).await;
            });
        }
        let stdout = tokio::spawn(drain(out));
        let stderr = tokio::spawn(drain(err));
        let waited = tokio::time::timeout(limit, child.wait()).await;
        let (exit_code, timed_out, error) = match waited {
            Ok(Ok(status)) => {
                // The program ended by itself; leave anything it started on
                // purpose alone, as the shell tool does.
                #[cfg(unix)]
                group.disarm();
                (status.code(), false, None)
            }
            Ok(Err(e)) => (
                None,
                false,
                Some(format!("waiting for the program failed: {e}")),
            ),
            Err(_) => {
                // Out of time: the whole group goes, then the program is reaped.
                #[cfg(unix)]
                drop(group);
                let _ = child.kill().await;
                (None, true, None)
            }
        };
        Report {
            exit_code,
            timed_out,
            error,
            stdout: finish(stdout).await,
            stderr: finish(stderr).await,
            elapsed: started.elapsed(),
        }
    }
}

#[async_trait]
impl Tool for GateCommandTool {
    fn name(&self) -> &str {
        GATE_TOOL
    }

    fn summary(&self) -> &str {
        "Run a workflow gate's command and report its exit status and output (harness only)."
    }

    fn description(&self) -> &str {
        "Runs an argv without a shell and reports exit_code, timed_out, error, stdout, stderr. \
         Called by a harness gate step, never by a model: no agent mode lists it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_GATE_TIMEOUT_SECS },
                "cwd": { "type": "string", "description": "Directory inside the workspace" },
                "stdin": { "type": "string", "description": "Text the command reads on standard input" }
            },
            "required": ["argv", "timeout_secs"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    /// Listed in no mode: the model is never offered this tool.
    fn modes(&self) -> &[AgentMode] {
        &[]
    }

    fn kernel_capability(&self) -> ToolCapability {
        ToolCapability::ExecuteShell
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let argv: Option<Vec<String>> =
            call.args
                .get("argv")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .map(|v| v.as_str().map(str::to_string))
                        .collect()
                });
        let Some(argv) = argv.filter(|a| a.first().is_some_and(|p| !p.is_empty())) else {
            return ToolOutput::err(&call.id, "`argv` must be a non-empty list of strings");
        };
        let Some(seconds) = call.args.get("timeout_secs").and_then(Value::as_u64) else {
            return ToolOutput::err(&call.id, "`timeout_secs` is required");
        };
        let seconds = seconds.clamp(1, MAX_GATE_TIMEOUT_SECS);
        let dir = match call.args.get("cwd").and_then(Value::as_str) {
            Some(requested) => match self.scope.resolve_for(call, requested) {
                Ok(dir) => dir,
                Err(refused) => return refused,
            },
            None => match self.scope.root() {
                Some(root) => root.to_path_buf(),
                None => return ToolOutput::err(&call.id, "the gate tool has no workspace root"),
            },
        };
        let stdin = match call.args.get("stdin") {
            None | Some(Value::Null) => None,
            Some(Value::String(text)) if text.len() <= MAX_STDIN_BYTES => Some(text.clone()),
            Some(Value::String(text)) => {
                return ToolOutput::err(
                    &call.id,
                    format!(
                        "`stdin` is {} bytes, more than the {MAX_STDIN_BYTES} allowed",
                        text.len()
                    ),
                );
            }
            Some(_) => return ToolOutput::err(&call.id, "`stdin` must be a string"),
        };
        let report = self
            .run(&argv, &dir, Duration::from_secs(seconds), stdin)
            .await;
        ToolOutput::ok(&call.id, report.to_json())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tool(dir: &tempfile::TempDir) -> GateCommandTool {
        GateCommandTool::new(dir.path()).expect("tool")
    }

    async fn run(tool: &GateCommandTool, args: Value) -> (bool, Value) {
        let out = tool
            .execute(&ToolCall {
                id: "g".into(),
                name: GATE_TOOL.into(),
                args,
            })
            .await;
        let report = serde_json::from_str(&out.content).unwrap_or(Value::String(out.content));
        (out.is_error, report)
    }

    #[tokio::test]
    async fn it_reports_the_exit_status_and_the_output_of_a_program() {
        let dir = tempfile::tempdir().unwrap();
        let (is_error, r) = run(
            &tool(&dir),
            json!({ "argv": ["sh", "-c", "echo out; echo err >&2; exit 3"], "timeout_secs": 10 }),
        )
        .await;
        assert!(!is_error, "a failing program is a report, not a tool error");
        assert_eq!(r["exit_code"], 3);
        assert_eq!(r["stdout"], "out\n");
        assert_eq!(r["stderr"], "err\n");
        assert_eq!(r["timed_out"], false);
        assert_eq!(r["error"], Value::Null);
    }

    #[tokio::test]
    async fn the_command_reads_the_text_it_is_given_on_standard_input() {
        let dir = tempfile::tempdir().unwrap();
        // Far more than an argument can hold, and with every awkward byte in it.
        let text = format!(
            "{}{{{{ not a template }}}} $HOME `x`\n",
            "line\n".repeat(400_000)
        );
        let (is_error, r) = run(
            &tool(&dir),
            json!({ "argv": ["wc", "-c"], "timeout_secs": 20, "stdin": text }),
        )
        .await;
        assert!(!is_error);
        assert_eq!(r["exit_code"], 0);
        assert_eq!(r["stdout"].as_str().unwrap().trim(), text.len().to_string());
    }

    #[tokio::test]
    async fn without_stdin_the_command_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r) = run(
            &tool(&dir),
            json!({ "argv": ["wc", "-c"], "timeout_secs": 10 }),
        )
        .await;
        assert_eq!(r["stdout"].as_str().unwrap().trim(), "0");
    }

    #[tokio::test]
    async fn a_command_that_ignores_its_input_does_not_hang_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(8 * 1024 * 1024);
        let (_, r) = run(
            &tool(&dir),
            json!({ "argv": ["true"], "timeout_secs": 20, "stdin": big }),
        )
        .await;
        assert_eq!(
            r["exit_code"], 0,
            "a closed pipe is not a failure of the gate: {r}"
        );
    }

    #[tokio::test]
    async fn success_is_exit_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r) = run(&tool(&dir), json!({ "argv": ["true"], "timeout_secs": 10 })).await;
        assert_eq!(r["exit_code"], 0);
    }

    #[tokio::test]
    async fn there_is_no_shell_to_interpolate_into() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r) = run(
            &tool(&dir),
            json!({ "argv": ["echo", "$HOME; touch pwned"], "timeout_secs": 10 }),
        )
        .await;
        assert_eq!(r["stdout"], "$HOME; touch pwned\n");
        assert!(!dir.path().join("pwned").exists());
    }

    #[tokio::test]
    async fn a_program_that_cannot_start_is_reported_as_an_error_without_a_status() {
        let dir = tempfile::tempdir().unwrap();
        let (is_error, r) = run(
            &tool(&dir),
            json!({ "argv": ["/nonexistent/program"], "timeout_secs": 10 }),
        )
        .await;
        assert!(!is_error);
        assert_eq!(r["exit_code"], Value::Null);
        assert!(r["error"]
            .as_str()
            .is_some_and(|e| e.contains("/nonexistent/program")));
    }

    #[tokio::test]
    async fn a_program_out_of_time_is_killed_with_everything_it_started() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survivor");
        let script = format!("(sleep 3; touch {}) & wait", marker.display());
        let started = Instant::now();
        let (_, r) = run(
            &tool(&dir),
            json!({ "argv": ["sh", "-c", script], "timeout_secs": 1 }),
        )
        .await;
        assert_eq!(r["timed_out"], true);
        assert_eq!(r["exit_code"], Value::Null);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "returned at the deadline"
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "the descendant was killed with the group");
    }

    #[tokio::test]
    async fn only_the_tail_of_long_output_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let (_, r) = run(
            &tool(&dir),
            json!({ "argv": ["sh", "-c", "yes line | head -c 200000; echo END"], "timeout_secs": 10 }),
        )
        .await;
        let out = r["stdout"].as_str().unwrap();
        assert!(out.len() <= STREAM_TAIL_BYTES);
        assert!(out.ends_with("END\n"));
        assert_eq!(r["truncated"], true);
    }

    #[tokio::test]
    async fn the_working_directory_stays_inside_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let tool = tool(&dir);
        let (is_error, r) = run(
            &tool,
            json!({ "argv": ["pwd"], "timeout_secs": 5, "cwd": "sub" }),
        )
        .await;
        assert!(!is_error);
        assert!(r["stdout"].as_str().unwrap().trim_end().ends_with("/sub"));
        for escape in ["..", "/", "sub/../../.."] {
            let (is_error, _) = run(
                &tool,
                json!({ "argv": ["pwd"], "timeout_secs": 5, "cwd": escape }),
            )
            .await;
            assert!(is_error, "cwd `{escape}` must be refused");
        }
    }

    #[tokio::test]
    async fn malformed_calls_are_tool_errors() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(&dir);
        for args in [
            json!({ "timeout_secs": 5 }),
            json!({ "argv": [], "timeout_secs": 5 }),
            json!({ "argv": [""], "timeout_secs": 5 }),
            json!({ "argv": [1], "timeout_secs": 5 }),
            json!({ "argv": ["true"] }),
        ] {
            let (is_error, _) = run(&tool, args.clone()).await;
            assert!(is_error, "{args}");
        }
    }

    #[test]
    fn no_agent_mode_lists_the_tool_and_it_needs_the_shell_capability() {
        let dir = tempfile::tempdir().unwrap();
        let tool = tool(&dir);
        assert!(tool.modes().is_empty());
        assert_eq!(tool.kernel_capability(), ToolCapability::ExecuteShell);
        assert_eq!(tool.name(), "shell_gate");
    }
}
