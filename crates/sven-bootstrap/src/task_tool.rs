// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TaskTool — spawns a full sven process as a focused sub-agent.
//!
//! Unlike the previous in-process implementation, this tool spawns the `sven`
//! binary with `--headless`, streams its stdout into an [`OutputBufferStore`]
//! entry, and returns a buffer handle immediately.  The parent agent then uses
//! `buf_status`, `buf_read`, and `buf_grep` to inspect the output without
//! loading it all into the context window.
//!
//! # Depth control
//!
//! Recursive spawning is limited by the `SVEN_SUBAGENT_DEPTH` environment
//! variable.  Each spawned process receives `SVEN_SUBAGENT_DEPTH=<parent+1>`.
//! When the depth reaches [`MAX_DEPTH`], the tool returns an error without
//! spawning.
//!
//! # Progress events
//!
//! While the subprocess runs, the background reader task emits
//! `ToolEvent::Progress` every [`PROGRESS_INTERVAL_MS`] milliseconds.  The
//! event contains a snapshot of the last few output lines so the TUI can
//! display live output in the expanded tool-call view.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use sven_config::AgentMode;
use sven_tools::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
    BufferSource, BufferStatus, OutputBufferStore,
};

/// Maximum subagent nesting depth (checked via `SVEN_SUBAGENT_DEPTH`).
const MAX_DEPTH: u32 = 3;

/// Environment variable used to track nesting depth across processes.
const DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

/// How often (in milliseconds) to send a `ToolEvent::Progress` snapshot while
/// the subprocess is running.
const PROGRESS_INTERVAL_MS: u64 = 500;

/// Number of trailing lines to include in each progress snapshot.
const PROGRESS_TAIL_LINES: usize = 20;

/// Spawns a full sven subprocess to execute a focused task, streams its output
/// into a shared [`OutputBufferStore`], and returns a buffer handle immediately.
pub struct TaskTool {
    /// Shared buffer store — same instance as registered for buf_read/buf_grep/buf_status.
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    /// Channel for sending tool progress events back to the agent loop.
    tool_event_tx: mpsc::Sender<ToolEvent>,
    /// Default model name forwarded to sub-agents (if any).  The user can
    /// override per-call by setting the `model` parameter.
    default_model: Option<String>,
}

impl TaskTool {
    pub fn new(
        buffer_store: Arc<Mutex<OutputBufferStore>>,
        tool_event_tx: mpsc::Sender<ToolEvent>,
        default_model: Option<String>,
    ) -> Self {
        Self {
            buffer_store,
            tool_event_tx,
            default_model,
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Spawn a focused sub-agent to complete an isolated task.  The sub-agent is a full sven \
         process running in headless mode.  This tool returns a buffer handle immediately — the \
         sub-agent keeps running in the background.\n\n\
         **Workflow:**\n\
         1. Call `task` to spawn the sub-agent and get a handle (e.g. `buf_001`).\n\
         2. Optionally call `task` again (with a different prompt) to spawn more sub-agents in parallel.\n\
         3. Poll with `buf_status` to check progress and when the sub-agent finishes.\n\
         4. Use `buf_grep` to locate specific sections (errors, results, identifiers).\n\
         5. Use `buf_read` to read specific line ranges in detail.\n\n\
         **When to use:**\n\
         - Exploring a large unfamiliar directory or codebase area\n\
         - Running a multi-step investigation that benefits from a clean context window\n\
         - Implementing a self-contained feature in a specific file/module\n\
         - Running tests, checking build output, or analysing failures in parallel\n\n\
         **When NOT to use:**\n\
         - Simple tasks you can do directly with one or two tool calls\n\
         - Tasks that require user interaction (sub-agents run headless)\n\n\
         Sub-agents have access to all standard tools.  Maximum nesting depth is 3."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Complete, self-contained task description for the sub-agent. \
                                    Include all context the sub-agent needs — it starts with a \
                                    fresh context window."
                },
                "description": {
                    "type": "string",
                    "description": "Short human-readable label for this sub-agent task (shown in TUI). \
                                    Example: 'Analyze auth module', 'Run test suite'"
                },
                "mode": {
                    "type": "string",
                    "enum": ["research", "plan", "agent"],
                    "description": "Operating mode for the sub-agent (default: agent)"
                },
                "workdir": {
                    "type": "string",
                    "description": "Working directory for the sub-agent process. \
                                    Defaults to the current working directory."
                },
                "model": {
                    "type": "string",
                    "description": "Model override for the sub-agent (e.g. 'fast' for a cheaper model). \
                                    Defaults to the same model as the parent."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'prompt'"),
        };

        let description = call
            .args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or(&prompt[..prompt.len().min(60)])
            .to_string();

        let mode = call
            .args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");

        let workdir: Option<PathBuf> = call
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let model_override = call
            .args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| self.default_model.clone());

        // Enforce depth limit.
        let current_depth: u32 = std::env::var(DEPTH_ENV)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if current_depth >= MAX_DEPTH {
            return ToolOutput::err(
                &call.id,
                format!(
                    "maximum sub-agent depth ({}) reached — cannot spawn further sub-agents",
                    MAX_DEPTH
                ),
            );
        }

        // Find the sven binary.
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("could not locate sven executable: {e}"))
            }
        };

        // Create the output buffer.
        let handle_id = {
            let mut store = self.buffer_store.lock().await;
            store.create(BufferSource::Subagent {
                prompt: prompt.clone(),
                mode: mode.to_string(),
                description: description.clone(),
            })
        };

        debug!(
            handle = %handle_id,
            prompt = %prompt,
            mode = %mode,
            depth = current_depth + 1,
            "task: spawning sub-agent process"
        );

        // Build the command.
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("--headless")
            .arg("--output-format")
            .arg("conversation")
            .arg("--mode")
            .arg(mode)
            .arg(&prompt)
            .env(DEPTH_ENV, (current_depth + 1).to_string())
            // Pass through the current environment so sub-agents can access
            // API keys and other env vars that the parent used for config expansion.
            .envs(std::env::vars())
            // Prevent the subprocess from opening a TUI.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Prevent the subprocess from inheriting our terminal
            .kill_on_drop(true);

        if let Some(ref m) = model_override {
            cmd.arg("--model").arg(m);
        }

        if let Some(ref wd) = workdir {
            cmd.current_dir(wd);
        }

        // Spawn the process.
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.buffer_store
                    .lock()
                    .await
                    .fail(&handle_id, format!("failed to spawn: {e}"));
                return ToolOutput::err(&call.id, format!("failed to spawn sub-agent: {e}"));
            }
        };

        // Record PID.
        if let Some(pid) = child.id() {
            self.buffer_store.lock().await.set_pid(&handle_id, pid);
        }

        // Launch background reader task.
        let store_clone = Arc::clone(&self.buffer_store);
        let event_tx_clone = self.tool_event_tx.clone();
        let handle_id_clone = handle_id.clone();
        let call_id = call.id.clone();

        tokio::spawn(async move {
            let stdout = child.stdout.take().expect("stdout piped");
            let stderr = child.stderr.take().expect("stderr piped");

            // Read stdout and stderr concurrently into the buffer.
            let store_out = Arc::clone(&store_clone);
            let hid_out = handle_id_clone.clone();
            let stdout_task = tokio::spawn(async move {
                let mut reader = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let line_bytes = format!("{}\n", line);
                    store_out
                        .lock()
                        .await
                        .append(&hid_out, line_bytes.as_bytes());
                }
            });

            let store_err = Arc::clone(&store_clone);
            let hid_err = handle_id_clone.clone();
            let stderr_task = tokio::spawn(async move {
                let mut reader = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = reader.next_line().await {
                    let line_bytes = format!("[stderr] {}\n", line);
                    store_err
                        .lock()
                        .await
                        .append(&hid_err, line_bytes.as_bytes());
                }
            });

            // Progress ticker: send status snapshots while running.
            let store_prog = Arc::clone(&store_clone);
            let hid_prog = handle_id_clone.clone();
            let call_id_prog = call_id.clone();
            let event_tx_prog = event_tx_clone.clone();
            let progress_task = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_millis(PROGRESS_INTERVAL_MS))
                        .await;

                    let (status, tail, lines) = {
                        let s = store_prog.lock().await;
                        let meta = match s.metadata(&hid_prog) {
                            Some(m) => m,
                            None => break,
                        };
                        let done = matches!(
                            meta.status,
                            BufferStatus::Finished { .. } | BufferStatus::Failed { .. }
                        );
                        let tail = s.tail(&hid_prog, PROGRESS_TAIL_LINES);
                        (done, tail, meta.total_lines)
                    };

                    let message = if tail.is_empty() {
                        format!("sub-agent running — {} lines", lines)
                    } else {
                        format!("[stream_buf:{}]\nlines:{}\n{}", hid_prog, lines, tail)
                    };

                    let _ = event_tx_prog
                        .send(ToolEvent::Progress {
                            call_id: call_id_prog.clone(),
                            message,
                        })
                        .await;

                    if status {
                        break;
                    }
                }
            });

            // Wait for all readers to finish, then wait for the process.
            let _ = tokio::join!(stdout_task, stderr_task);
            progress_task.abort();

            let exit_status = child.wait().await;
            match exit_status {
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    store_clone.lock().await.finish(&handle_id_clone, code);

                    // Final progress event with completion status.
                    let final_msg = {
                        let s = store_clone.lock().await;
                        let meta = s.metadata(&handle_id_clone);
                        meta.map(|m| {
                            format!(
                                "sub-agent finished (exit {code}) — {} lines, {:.1}s",
                                m.total_lines, m.elapsed_secs
                            )
                        })
                        .unwrap_or_else(|| format!("sub-agent finished (exit {code})"))
                    };
                    let _ = event_tx_clone
                        .send(ToolEvent::Progress {
                            call_id,
                            message: final_msg,
                        })
                        .await;
                }
                Err(e) => {
                    warn!("task: failed to wait for sub-agent process: {e}");
                    store_clone
                        .lock()
                        .await
                        .fail(&handle_id_clone, format!("process wait failed: {e}"));
                }
            }
        });

        // Return handle immediately — the model uses buf_status/buf_read/buf_grep.
        let json_result = json!({
            "handle": handle_id,
            "status": "running",
            "description": description,
        });

        ToolOutput::ok(
            &call.id,
            format!(
                "Sub-agent spawned.\n\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\
                 Status: running\n\n\
                 Use `buf_status` to check progress, `buf_grep` to search the output, \
                 and `buf_read` to read specific line ranges.\n\n\
                 Raw JSON: {json_result}",
                handle_id = handle_id,
                description = description,
                json_result = json_result,
            ),
        )
    }
}
