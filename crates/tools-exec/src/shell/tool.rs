// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for token-efficient, bounded
// command execution for LLM agents for its clients. If your team needs
// expertise in running build and test commands for agents then you can
// procure our services by sending an email to info@swedishembedded.com.
use async_trait::async_trait;
#[cfg(unix)]
use libc;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::debug;

use sven_hsm::ToolCapability;

use sven_tool_api::policy::ApprovalPolicy;
use sven_tool_api::tool::{OutputCategory, Tool, ToolCall, ToolDisplay, ToolOutput};
use sven_tool_api::PathScope;
use sven_tool_registry::ToolPolicy;
use sven_tools_fs::{BufferSource, OutputBufferStore};

use super::capture::{run_child, Exit, Sink};
use super::render::{
    render, BufferAccess, Captured, Footer, Limits, OutputMode, DEFAULT_VISIBLE_BYTES,
    DEFAULT_VISIBLE_LINES, MAX_VISIBLE_BYTES, MAX_VISIBLE_LINES,
};

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

/// Bytes of one command's output the session keeps for `buf_read`/`buf_grep`
/// unless [`ShellTool::capture_quota_bytes`] says otherwise.
pub const DEFAULT_CAPTURE_QUOTA_BYTES: usize = sven_tools_fs::DEFAULT_CAPTURE_QUOTA_BYTES;

/// Settings of every command, so a command that expects a person at the
/// other end (a pager, a credential prompt, an editor, colour codes) neither
/// hangs nor spends tokens on escape sequences.
const NON_INTERACTIVE_ENV: [(&str, &str); 6] = [
    ("CI", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GIT_EDITOR", "true"),
    ("NO_COLOR", "1"),
];

/// Why a launcher and a confinement cannot both be set.
pub const LAUNCHER_WITH_CONFINEMENT: &str =
    "tools.launcher and tools.confine are mutually exclusive: a launcher runs commands in the \
     application's own sandbox, and confining the launcher would confine the sandbox manager, \
     not the command";

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
    /// The session's output buffers. Every command's whole output is kept
    /// here under a handle, so the model can fetch what its result left out.
    /// One store per session: a handle is meaningless in another session.
    pub buffers: Arc<Mutex<OutputBufferStore>>,
    /// Bytes of one command's output kept in its buffer; the rest is counted
    /// and drained but not retained.
    pub capture_quota_bytes: usize,
    /// Bytes a result may carry, footer included, when the call does not ask
    /// for more lines than the default; never above 64 KiB.
    pub visible_bytes: usize,
    /// Whether the session has a way to read a buffer (the `task` tool's
    /// `read` and `grep` actions). Only then does a result name its buffer
    /// and say how to read it.
    pub buffers_readable: bool,
    /// Directories that go first on the `PATH` of every command, so a
    /// program that lives only there is found by its bare name. Set by the
    /// host that brought the programs, never read from a configuration file.
    pub path_prefix: Vec<std::path::PathBuf>,
    /// When set, every command runs inside a kernel confinement that reaches
    /// only these directories and no network, and is refused where the
    /// kernel cannot enforce it. Set by the host that runs untrusted
    /// commands beside trusted work, never read from a configuration file.
    pub confine: Option<crate::confine::CommandConfinement>,
    /// `[program, args...]`: every command is started as `program args...
    /// bash -c <command>`, in the call's working directory, instead of as
    /// `bash -c <command>`; the exit status is the launcher's. It is how an
    /// application runs commands in its own sandbox. Set by the host, never
    /// read from a configuration file, and refused together with
    /// [`Self::confine`]: Landlock on the launcher would confine the sandbox
    /// manager, not the command.
    pub launcher: Option<Vec<String>>,
    /// The `TMPDIR` of every command: the session's scratch directory, so
    /// temporary files stay the session's own instead of the shared `/tmp`.
    /// A [`Self::confine`] sets its own and wins.
    pub tmpdir: Option<std::path::PathBuf>,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_timeout_secs: DEFAULT_MAX_TIMEOUT_SECS,
            scope: PathScope::default(),
            policy: Arc::default(),
            buffers: Arc::default(),
            capture_quota_bytes: DEFAULT_CAPTURE_QUOTA_BYTES,
            visible_bytes: DEFAULT_VISIBLE_BYTES,
            buffers_readable: false,
            path_prefix: Vec::new(),
            confine: None,
            launcher: None,
            tmpdir: None,
        }
    }
}

impl ShellTool {
    /// This tool, handing `dir` to every command as its `TMPDIR`.
    #[must_use]
    pub fn with_tmpdir(mut self, dir: Option<std::path::PathBuf>) -> Self {
        self.tmpdir = dir;
        self
    }

    /// Keep this tool's output in a session's `buffers`. `readable` says the
    /// session can read a buffer back (the `task` tool's `read` and `grep`
    /// actions), so a result names its buffer and how to read it.
    #[must_use]
    pub fn with_buffers(mut self, buffers: Arc<Mutex<OutputBufferStore>>, readable: bool) -> Self {
        self.buffers = buffers;
        self.buffers_readable = readable;
        self
    }

    /// How much of the output `call` asks to see.
    fn limits(&self, call: &ToolCall) -> Result<Limits, String> {
        let mode = match call.args.get("output_mode").and_then(Value::as_str) {
            None => OutputMode::Auto,
            Some(name) => OutputMode::parse(name).ok_or_else(|| {
                format!(
                    "unknown output_mode '{name}': use one of {}",
                    OutputMode::NAMES
                )
            })?,
        };
        let asked = call.args.get("max_lines").and_then(Value::as_u64);
        let lines = asked.map_or(DEFAULT_VISIBLE_LINES, |n| {
            usize::try_from(n)
                .unwrap_or(usize::MAX)
                .clamp(1, MAX_VISIBLE_LINES)
        });
        // Asking for more lines than the default is asking to spend more
        // tokens, so the byte budget grows with it; otherwise 1,000 lines
        // would be cut back to a few dozen.
        let bytes = if lines > DEFAULT_VISIBLE_LINES {
            MAX_VISIBLE_BYTES
        } else {
            self.visible_bytes.clamp(1024, MAX_VISIBLE_BYTES)
        };
        Ok(Limits { lines, bytes, mode })
    }

    /// Run `command` and return how it ended; its output goes to `sink`.
    async fn spawn_and_capture(
        &self,
        command: &str,
        workdir: Option<&std::path::Path>,
        timeout: Duration,
        sink: &Sink,
    ) -> Result<super::capture::Outcome, String> {
        // On Unix/macOS: bash -c <command>
        // On Windows:    cmd /C <command>  (PowerShell is too verbose for tool use)
        #[cfg(unix)]
        let mut cmd = match &self.launcher {
            None => {
                let mut c = Command::new("bash");
                c.arg("-c").arg(command);
                c
            }
            Some(_) if self.confine.is_some() => return Err(LAUNCHER_WITH_CONFINEMENT.into()),
            Some(launcher) => {
                let (program, args) = launcher
                    .split_first()
                    .ok_or("the command launcher names no program")?;
                let mut c = Command::new(program);
                c.args(args).args(["bash", "-c", command]);
                c
            }
        };
        #[cfg(windows)]
        let mut cmd = {
            if self.launcher.is_some() {
                return Err("tools.launcher is not supported on Windows".into());
            }
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
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
        // A command that reads stdin sees end of file at once.
        //
        // `kill_on_drop(true)` kills the shell itself when the child handle is
        // dropped; `capture::run_child` kills the rest of its process group on
        // a timeout and when the call is dropped (a cancelled turn), so a
        // command's own children do not outlive the call and keep touching the
        // terminal or the workspace.
        if let Some(confinement) = &self.confine {
            confinement.restrict(cmd.as_std_mut())?;
        }
        cmd.stdin(Stdio::null());
        cmd.kill_on_drop(true);
        cmd.envs(NON_INTERACTIVE_ENV);
        if let Some(path) = crate::search_path::with_prefix(&self.path_prefix)? {
            cmd.env("PATH", path);
        }
        if let (None, Some(tmp)) = (&self.confine, &self.tmpdir) {
            cmd.env("TMPDIR", tmp);
        }
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
        if let Some(wd) = workdir {
            cmd.current_dir(wd);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = cmd.spawn().map_err(|e| format!("spawn error: {e}"))?;
        if let Some(pid) = child.id() {
            self.buffers.lock().await.set_pid(&sink.handle, pid);
        }
        Ok(run_child(child, timeout, sink).await)
    }

    /// What the buffer `handle` holds, for rendering `limits.lines` of it.
    async fn captured(&self, handle: &str, lines: usize) -> Captured {
        let store = self.buffers.lock().await;
        let meta = store.metadata(handle);
        Captured {
            head: store.lines(handle, 0, lines).unwrap_or_default(),
            tail: store.tail_lines(handle, lines).unwrap_or_default(),
            observed_lines: meta.as_ref().map_or(0, |m| m.observed_lines),
            retained_lines: meta.as_ref().map_or(0, |m| m.total_lines),
            observed_bytes: meta.as_ref().map_or(0, |m| m.observed_bytes),
            retained_bytes: meta.as_ref().map_or(0, |m| m.total_bytes),
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
        "Run a non-interactive shell command (bash; cmd.exe on Windows); stdin is empty,\n\
         so prompts fail. Shows the first 20 and last 60 lines, then the exit code; the\n\
         rest is kept under a buffer handle. Filter at the source or raise max_lines.\n\
         Use it for builds, tests, git and running programs; pass timeout_secs for builds,\n\
         test suites and other long runs."
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
                    "description": "Default: cwd"
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": format!(
                        "Seconds before kill (default {}, max {})",
                        self.timeout_secs.min(self.max_timeout_secs),
                        self.max_timeout_secs
                    )
                },
                "max_lines": {
                    "type": "integer",
                    "description": format!(
                        "Lines shown (default {DEFAULT_VISIBLE_LINES}, max {MAX_VISIBLE_LINES})"
                    )
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["auto", "head", "tail", "head_tail"],
                    "description": "auto: first 25%, last 75%"
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
        // The result already fits its budget and ends with a footer (exit
        // code, buffer handle) that the model needs; global truncation must
        // cut the body, never the footer.
        OutputCategory::Bounded
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
        let limits = match self.limits(call) {
            Ok(limits) => limits,
            Err(refusal) => return ToolOutput::err(&call.id, refusal),
        };
        let workdir = match call.args.get("workdir").and_then(|v| v.as_str()) {
            Some(requested) => match self.scope.resolve_for(call, requested) {
                Ok(dir) => Some(dir),
                Err(refused) => return refused,
            },
            None => self.scope.root().map(std::path::Path::to_path_buf),
        };
        let timeout_secs = call
            .args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.timeout_secs)
            .min(self.max_timeout_secs);

        debug!(cmd = %command, "executing shell tool");

        let handle = self.buffers.lock().await.create_with_quota(
            BufferSource::Shell {
                command: command.clone(),
                workdir: workdir
                    .as_deref()
                    .map_or_else(|| ".".into(), |d| d.display().to_string()),
            },
            self.capture_quota_bytes,
        );
        let sink = Sink {
            store: Arc::clone(&self.buffers),
            handle,
        };
        let outcome = match self
            .spawn_and_capture(
                &command,
                workdir.as_deref(),
                Duration::from_secs(timeout_secs),
                &sink,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                self.buffers.lock().await.fail(&sink.handle, e.clone());
                return ToolOutput::err(&call.id, e);
            }
        };
        {
            let mut store = self.buffers.lock().await;
            match &outcome.exit {
                Exit::Code(code) => store.finish(&sink.handle, *code),
                other => store.fail(&sink.handle, other.describe()),
            }
        }

        let captured = self.captured(&sink.handle, limits.lines).await;
        let silent = captured.observed_bytes == 0;
        let footer = Footer {
            exit: outcome.exit.describe(),
            duration: outcome.elapsed,
            note: matches!(outcome.exit, Exit::TimedOut).then(|| {
                format!(
                    "timeout after {timeout_secs}s: the command was killed. The limit used was \
                     {timeout_secs}s; the longest a call may ask for is {max}s \
                     (tools.max_timeout_secs). Retry with a larger timeout_secs (up to {max}) if it \
                     needs the time, or take a different approach (smaller steps, a narrower \
                     command, running it in the background).",
                    max = self.max_timeout_secs
                )
            }),
            buffer: self.buffers_readable.then_some(BufferAccess {
                handle: &sink.handle,
            }),
        };
        let content = render(&captured, limits, &footer);
        if outcome.exit.is_failure(silent) {
            ToolOutput::err(&call.id, content)
        } else {
            ToolOutput::ok(&call.id, content)
        }
    }
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
