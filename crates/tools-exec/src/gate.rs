// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The gate command tool: run commands in order, report how they ended.
//!
//! This is the process half of a harness `gate` step. It is a tool that no
//! agent mode lists ([`Tool::modes`] is empty) and that the harness runtime
//! never admits to an agent step, so only a workflow's own gates run it. It
//! runs each program directly - the tool has no shell of its own; a script
//! gate hands its script to the shell it names as an argument - in a new
//! session with its working directory confined to the workspace, kills the
//! whole process group when the time is up, and reports the facts: exit
//! status, whether the time ran out, whether the program could be started at
//! all, and the end of each output stream. A gate of several steps runs them
//! one after the other under one deadline and stops at the first that does not
//! end with status 0; the report is that step's (or the last one's) and names
//! the step that failed. It does not decide whether that is success; the
//! harness machine does.
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

/// The most a gate may ask to keep of each stream (`tail_bytes`).
const MAX_TAIL_BYTES: usize = 1024 * 1024;

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

/// Reads `reader` to its end into `tail`, keeping the last `keep` bytes. The
/// tail is shared so that what was read survives a reader that never sees
/// the end (a descendant of the program holding the pipe open).
async fn drain<R: AsyncRead + Unpin>(
    mut reader: R,
    keep: usize,
    tail: std::sync::Arc<std::sync::Mutex<Tail>>,
) {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut tail = tail
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                tail.bytes.extend_from_slice(&chunk[..n]);
                if tail.bytes.len() > 2 * keep {
                    let drop = tail.bytes.len() - keep;
                    tail.bytes.drain(..drop);
                    tail.truncated = true;
                }
            }
        }
    }
}

/// What a stream's reader kept, once it reached the end or the grace after
/// the program ended ran out: a stream a descendant still holds open keeps
/// what was read so far rather than losing it.
async fn finish(
    task: tokio::task::JoinHandle<()>,
    tail: std::sync::Arc<std::sync::Mutex<Tail>>,
    keep: usize,
) -> Tail {
    if tokio::time::timeout(DRAIN_GRACE, task).await.is_err() {
        // The reader is abandoned with the pipe; what it read stays.
    }
    let mut tail = std::mem::take(
        &mut *tail
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if tail.bytes.len() > keep {
        let drop = tail.bytes.len() - keep;
        tail.bytes.drain(..drop);
        tail.truncated = true;
    }
    tail
}

/// The most steps one gate may have.
const MAX_STEPS: usize = 64;

/// One step of a call: the name its result carries, and the program to run.
struct Step {
    name: String,
    argv: Vec<String>,
}

/// The report of one step, in the shape the harness machine reads.
struct Report {
    exit_code: Option<i32>,
    timed_out: bool,
    error: Option<String>,
    stdout: Tail,
    stderr: Tail,
    elapsed: Duration,
}

impl Report {
    /// A step that never started because the gate's time was already spent.
    fn out_of_time() -> Self {
        Self {
            exit_code: None,
            timed_out: true,
            error: None,
            stdout: Tail::default(),
            stderr: Tail::default(),
            elapsed: Duration::ZERO,
        }
    }

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

    /// `true` when the step ended by itself with status 0.
    fn passed(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && self.error.is_none()
    }
}

fn millis(elapsed: Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

/// The report of a whole gate: the steps that ran, in order. The last one
/// is the one that ended the gate.
struct GateReport {
    steps: Vec<(String, Report)>,
}

impl GateReport {
    fn to_json(&self) -> String {
        let text = |tail: &Tail| String::from_utf8_lossy(&tail.bytes).into_owned();
        let last = self.steps.last().map(|(_, report)| report);
        let failed = self
            .steps
            .iter()
            .find(|(_, report)| !report.passed())
            .map(|(name, _)| name);
        json!({
            "exit_code": last.and_then(|r| r.exit_code),
            "timed_out": last.is_some_and(|r| r.timed_out),
            "error": last.and_then(|r| r.error.clone()),
            "stdout": last.map(|r| text(&r.stdout)).unwrap_or_default(),
            "stderr": last.map(|r| text(&r.stderr)).unwrap_or_default(),
            "truncated": last.is_some_and(|r| r.stdout.truncated || r.stderr.truncated),
            "duration_ms": millis(self.steps.iter().map(|(_, r)| r.elapsed).sum()),
            "failed_step": failed,
            "steps": self.steps.iter().map(|(name, report)| json!({
                "name": name,
                "exit_code": report.exit_code,
                "duration_ms": millis(report.elapsed),
            })).collect::<Vec<_>>(),
        })
        .to_string()
    }
}

/// The steps of a call: a non-empty list of `{name, argv}`, each argv a
/// non-empty list of strings whose first element names the program.
fn steps(given: Option<&Value>) -> Result<Vec<Step>, String> {
    let bad = "`steps` must be a list of {name, argv}: a name and a non-empty list of strings";
    let items = given.and_then(Value::as_array).ok_or(bad)?;
    if items.is_empty() || items.len() > MAX_STEPS {
        return Err(format!("`steps` must hold 1 to {MAX_STEPS} steps"));
    }
    items
        .iter()
        .map(|item| {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty());
            let argv: Option<Vec<String>> = item
                .get("argv")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect());
            match (name, argv) {
                (Some(name), Some(argv)) if argv.first().is_some_and(|p| !p.is_empty()) => {
                    Ok(Step {
                        name: name.to_string(),
                        argv,
                    })
                }
                _ => Err(bad.to_string()),
            }
        })
        .collect()
}

/// The variables a call sets for its command: an object of string values
/// whose names are plain identifiers (`[A-Za-z_][A-Za-z0-9_]*`).
fn environment(given: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let entries = match given {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Object(entries)) => entries,
        Some(_) => return Err("`env` must be an object of strings".to_string()),
    };
    entries
        .iter()
        .map(|(name, value)| {
            let plain = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && !name.is_empty();
            match (plain, value.as_str()) {
                (true, Some(text)) => Ok((name.clone(), text.to_string())),
                (false, _) => Err(format!("`env` name `{name}` is not a plain variable name")),
                (true, None) => Err(format!("`env` value of `{name}` must be a string")),
            }
        })
        .collect()
}

/// Runs a harness gate's command inside the workspace.
pub struct GateCommandTool {
    scope: PathScope,
    trajectory: Option<TrajectoryHook>,
}

/// Writes the run's trajectory so far to the file a gate is about to read.
/// The host supplies it: only the host can see the run's conversation.
pub type TrajectoryHook =
    std::sync::Arc<dyn Fn(&std::path::Path) -> Result<(), String> + Send + Sync>;

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
            trajectory: None,
        })
    }

    /// Lets a gate read the run's trajectory: a call that names a
    /// `trajectory_file` has it written by `hook` first, and fails closed
    /// when there is no hook or it fails.
    #[must_use]
    pub fn with_trajectory_hook(mut self, hook: TrajectoryHook) -> Self {
        self.trajectory = Some(hook);
        self
    }

    /// The directory for a call that names a trusted one: absolute, an
    /// existing directory, and disjoint from the workspace. The workspace is
    /// what the agent can write, so a directory in it (or holding it) would
    /// let the agent change what its own check runs against.
    fn trusted_dir(
        &self,
        call: &ToolCall,
        requested: &str,
    ) -> Result<std::path::PathBuf, ToolOutput> {
        let refuse = |why: String| ToolOutput::err(&call.id, why);
        let path = std::path::Path::new(requested);
        if !path.is_absolute() {
            return Err(refuse(format!(
                "`trusted_cwd` {requested} must be an absolute path"
            )));
        }
        let real = path
            .canonicalize()
            .map_err(|e| refuse(format!("`trusted_cwd` {requested}: {e}")))?;
        if !real.is_dir() {
            return Err(refuse(format!(
                "`trusted_cwd` {requested} is not a directory"
            )));
        }
        if let Some(root) = self.scope.root().and_then(|r| r.canonicalize().ok()) {
            if real.starts_with(&root) || root.starts_with(&real) {
                return Err(refuse(format!(
                    "`trusted_cwd` {requested} overlaps the workspace, which the agent can write"
                )));
            }
        }
        Ok(real)
    }

    /// Runs the steps in order under one deadline, up to the first that does
    /// not pass. A step that finds the time already spent is reported as
    /// timed out without being started.
    async fn run(
        &self,
        steps: &[Step],
        dir: &std::path::Path,
        limit: Duration,
        stdin: Option<&str>,
        keep: usize,
        env: &[(String, String)],
    ) -> GateReport {
        let deadline = Instant::now() + limit;
        let mut ran = Vec::new();
        for step in steps {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let report = if remaining.is_zero() {
                Report::out_of_time()
            } else {
                self.run_step(
                    &step.argv,
                    dir,
                    remaining,
                    stdin.map(str::to_string),
                    keep,
                    env,
                )
                .await
            };
            let passed = report.passed();
            ran.push((step.name.clone(), report));
            if !passed {
                break;
            }
        }
        GateReport { steps: ran }
    }

    async fn run_step(
        &self,
        argv: &[String],
        dir: &std::path::Path,
        limit: Duration,
        stdin: Option<String>,
        keep: usize,
        env: &[(String, String)],
    ) -> Report {
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(dir)
            .envs(env.iter().map(|(name, value)| (name, value)))
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
        let (out_tail, err_tail) = (Default::default(), Default::default());
        let stdout = tokio::spawn(drain(out, keep, std::sync::Arc::clone(&out_tail)));
        let stderr = tokio::spawn(drain(err, keep, std::sync::Arc::clone(&err_tail)));
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
            stdout: finish(stdout, out_tail, keep).await,
            stderr: finish(stderr, err_tail, keep).await,
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
        "Runs commands in order (each an argv, without a shell of its own) and reports exit_code, \
         timed_out, error, stdout, stderr of the step that ended the gate, with failed_step and \
         each step's duration. Called by a harness gate step, never by a model: no agent mode \
         lists it."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "steps": {
                    "type": "array",
                    "description": "The commands, run in order; the first that does not exit 0 ends the gate",
                    "minItems": 1,
                    "maxItems": MAX_STEPS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "minLength": 1, "description": "What the report calls the step" },
                            "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1 }
                        },
                        "required": ["name", "argv"],
                        "additionalProperties": false
                    }
                },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": MAX_GATE_TIMEOUT_SECS },
                "cwd": { "type": "string", "description": "Directory inside the workspace" },
                "stdin": { "type": "string", "description": "Text the command reads on standard input" },
                "tail_bytes": { "type": "integer", "minimum": 1024, "maximum": MAX_TAIL_BYTES, "description": "How much of the end of each output stream to keep (default 16384)" },
                "trusted_cwd": { "type": "string", "description": "Absolute directory outside the workspace (a run input the host set)" },
                "trajectory_file": { "type": "string", "description": "File the host writes the run's trajectory to before the command starts" },
                "env": { "type": "object", "description": "Variables set for the command, name to value", "additionalProperties": { "type": "string" } }
            },
            "required": ["steps", "timeout_secs"],
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
        let steps = match steps(call.args.get("steps")) {
            Ok(steps) => steps,
            Err(why) => return ToolOutput::err(&call.id, why),
        };
        let Some(seconds) = call.args.get("timeout_secs").and_then(Value::as_u64) else {
            return ToolOutput::err(&call.id, "`timeout_secs` is required");
        };
        let seconds = seconds.clamp(1, MAX_GATE_TIMEOUT_SECS);
        if let Some(file) = call.args.get("trajectory_file").and_then(Value::as_str) {
            match &self.trajectory {
                Some(write) => {
                    if let Err(why) = write(std::path::Path::new(file)) {
                        return ToolOutput::err(
                            &call.id,
                            format!("cannot write the trajectory for the gate: {why}"),
                        );
                    }
                }
                None => {
                    return ToolOutput::err(&call.id, "this host cannot write the run's trajectory")
                }
            }
        }
        let trusted = call.args.get("trusted_cwd").and_then(Value::as_str);
        let dir = match (call.args.get("cwd").and_then(Value::as_str), trusted) {
            (Some(_), Some(_)) => {
                return ToolOutput::err(&call.id, "`cwd` and `trusted_cwd` are exclusive")
            }
            (None, Some(requested)) => match self.trusted_dir(call, requested) {
                Ok(dir) => dir,
                Err(refused) => return refused,
            },
            (Some(requested), None) => match self.scope.resolve_for(call, requested) {
                Ok(dir) => dir,
                Err(refused) => return refused,
            },
            (None, None) => match self.scope.root() {
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
        let keep = match call.args.get("tail_bytes") {
            None | Some(Value::Null) => STREAM_TAIL_BYTES,
            Some(v) => match v.as_u64().and_then(|n| usize::try_from(n).ok()) {
                Some(n) if (1024..=MAX_TAIL_BYTES).contains(&n) => n,
                _ => {
                    return ToolOutput::err(
                        &call.id,
                        format!(
                            "`tail_bytes` must be a whole number within 1024..={MAX_TAIL_BYTES}"
                        ),
                    )
                }
            },
        };
        let env = match environment(call.args.get("env")) {
            Ok(env) => env,
            Err(why) => return ToolOutput::err(&call.id, why),
        };
        let report = self
            .run(
                &steps,
                &dir,
                Duration::from_secs(seconds),
                stdin.as_deref(),
                keep,
                &env,
            )
            .await;
        ToolOutput::ok(&call.id, report.to_json())
    }
}

#[cfg(all(test, unix))]
mod tests;
