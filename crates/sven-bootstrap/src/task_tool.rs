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
    BufGrepTool, BufReadTool, BufStatusTool, BufferSource, BufferStatus, OutputBufferStore,
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
///
/// Also serves as the buffer inspection interface (absorbs buf_status, buf_read,
/// buf_grep) via the `action` parameter to reduce total tool count.
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
        "Spawn a focused sub-agent or inspect a running sub-agent's output.\n\
         action: spawn (default) | status | read | grep\n\n\
         **Spawn workflow (action=spawn or omitted):**\n\
         1. Call `task` with prompt → get a buffer handle (e.g. buf_0001)\n\
         2. Optionally spawn more sub-agents in parallel with different prompts\n\
         3. Poll with `task(action=status, handle=...)` to check progress\n\
         4. Use `task(action=grep, ...)` to locate sections in the output\n\
         5. Use `task(action=read, ...)` to read specific line ranges\n\n\
         **When to spawn:**\n\
         - Exploring a large unfamiliar area or running a multi-step investigation\n\
         - Implementing a self-contained feature in a specific file/module\n\
         - Running tests, build output, or analyses in parallel\n\n\
         Sub-agents have access to all standard tools. Maximum nesting depth is 3."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["spawn", "status", "read", "grep"],
                    "description": "spawn (default): launch sub-agent; status/read/grep: inspect existing buffer"
                },
                "prompt": {
                    "type": "string",
                    "description": "[action=spawn] Complete, self-contained task description for the sub-agent"
                },
                "description": {
                    "type": "string",
                    "description": "[action=spawn] Short human-readable label (shown in TUI)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["research", "plan", "agent"],
                    "description": "[action=spawn] Operating mode for the sub-agent (default: agent)"
                },
                "workdir": {
                    "type": "string",
                    "description": "[action=spawn] Working directory (defaults to current)"
                },
                "model": {
                    "type": "string",
                    "description": "[action=spawn] Model override (e.g. 'fast')"
                },
                "handle": {
                    "type": "string",
                    "description": "[action=status|read|grep] Buffer handle from a previous spawn"
                },
                "start_line": {
                    "type": "integer",
                    "description": "[action=read] First line to read (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "[action=read] Last line to read (inclusive)"
                },
                "pattern": {
                    "type": "string",
                    "description": "[action=grep] Regex pattern to search for in the buffer"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "[action=grep] Lines of context before/after each match (default 2)"
                },
                "limit": {
                    "type": "integer",
                    "description": "[action=grep] Max matches (default 50)"
                }
            },
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
        let action = call
            .args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("spawn");

        match action {
            "status" => return self.execute_status(call).await,
            "read" => return self.execute_read(call).await,
            "grep" => return self.execute_grep(call).await,
            _ => {}
        }

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

        // Notify TUI so it can create a child session and show the subagent in the tree.
        let _ = self
            .tool_event_tx
            .send(ToolEvent::SubagentStarted {
                call_id: call.id.clone(),
                handle_id: handle_id.clone(),
                description: description.clone(),
            })
            .await;

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
                 Use task(action=status, handle=...) to check progress,\n\
                 task(action=grep, ...) to search output,\n\
                 task(action=read, ...) to read specific line ranges.\n\n\
                 Raw JSON: {json_result}",
                handle_id = handle_id,
                description = description,
                json_result = json_result,
            ),
        )
    }
}

impl TaskTool {
    async fn execute_status(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "missing required parameter 'handle' for action=status",
                )
            }
        };
        let delegate_call = ToolCall {
            id: call.id.clone(),
            name: "buf_status".into(),
            args: serde_json::json!({ "handle": handle }),
        };
        let buf_status = BufStatusTool::new(self.buffer_store.clone());
        buf_status.execute(&delegate_call).await
    }

    async fn execute_read(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "missing required parameter 'handle' for action=read",
                )
            }
        };
        let start_line = call
            .args
            .get("start_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(1);
        let end_line = call
            .args
            .get("end_line")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);
        let delegate_call = ToolCall {
            id: call.id.clone(),
            name: "buf_read".into(),
            args: serde_json::json!({
                "handle": handle,
                "start_line": start_line,
                "end_line": end_line
            }),
        };
        let buf_read = BufReadTool::new(self.buffer_store.clone());
        buf_read.execute(&delegate_call).await
    }

    async fn execute_grep(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) => h.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "missing required parameter 'handle' for action=grep",
                )
            }
        };
        let pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                return ToolOutput::err(
                    &call.id,
                    "missing required parameter 'pattern' for action=grep",
                )
            }
        };
        let context_lines = call
            .args
            .get("context_lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(2);
        let limit = call
            .args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50);
        let delegate_call = ToolCall {
            id: call.id.clone(),
            name: "buf_grep".into(),
            args: serde_json::json!({
                "handle": handle,
                "pattern": pattern,
                "context_lines": context_lines,
                "limit": limit
            }),
        };
        let buf_grep = BufGrepTool::new(self.buffer_store.clone());
        buf_grep.execute(&delegate_call).await
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    use sven_tools::{
        tool::{Tool, ToolCall},
        OutputBufferStore,
    };

    use super::TaskTool;

    fn make_task() -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        TaskTool::new(store, tx, None)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "task".into(),
            args,
        }
    }

    #[test]
    fn name_is_task() {
        assert_eq!(make_task().name(), "task");
    }

    #[tokio::test]
    async fn status_action_missing_handle_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "status"}))).await;
        assert!(out.is_error, "expected error, got: {}", out.content);
        assert!(
            out.content.contains("handle"),
            "error should mention 'handle'"
        );
    }

    #[tokio::test]
    async fn read_action_missing_handle_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "read"}))).await;
        assert!(out.is_error, "expected error, got: {}", out.content);
        assert!(
            out.content.contains("handle"),
            "error should mention 'handle'"
        );
    }

    #[tokio::test]
    async fn grep_action_missing_handle_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "pattern": "foo"})))
            .await;
        assert!(out.is_error, "expected error, got: {}", out.content);
        assert!(
            out.content.contains("handle"),
            "error should mention 'handle'"
        );
    }

    #[tokio::test]
    async fn grep_action_missing_pattern_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "handle": "buf_0001"})))
            .await;
        assert!(out.is_error, "expected error, got: {}", out.content);
        assert!(
            out.content.contains("pattern"),
            "error should mention 'pattern'"
        );
    }

    #[tokio::test]
    async fn status_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "status", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error, "expected error for unknown handle");
    }

    #[tokio::test]
    async fn read_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "read", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error, "expected error for unknown handle");
    }

    #[tokio::test]
    async fn grep_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(
                json!({"action": "grep", "handle": "buf_9999", "pattern": "foo"}),
            ))
            .await;
        assert!(out.is_error, "expected error for unknown handle");
    }
}

// ─── Adversarial tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod adversarial_tests {
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    use sven_tools::{
        tool::{Tool, ToolCall},
        OutputBufferStore,
    };

    use super::{TaskTool, DEPTH_ENV, MAX_DEPTH};

    // Serialize tests that mutate the SVEN_SUBAGENT_DEPTH env var to avoid
    // race conditions when Rust runs tests in parallel async tasks.
    // tokio::sync::Mutex can be held across .await points without deadlocking.
    static DEPTH_ENV_MUTEX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn make_task() -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        TaskTool::new(store, tx, None)
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "adv".into(),
            name: "task".into(),
            args,
        }
    }

    #[tokio::test]
    async fn spawn_missing_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "spawn"}))).await;
        assert!(
            out.is_error,
            "missing prompt should be an error: {}",
            out.content
        );
        assert!(
            out.content.contains("prompt"),
            "error should name missing field"
        );
    }

    #[tokio::test]
    async fn spawn_null_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": null}))).await;
        assert!(
            out.is_error,
            "null prompt should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn spawn_integer_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": 42}))).await;
        assert!(
            out.is_error,
            "integer prompt should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn spawn_blocked_at_max_depth() {
        let _guard = DEPTH_ENV_MUTEX.lock().await;
        // Simulate being at the maximum allowed depth.
        std::env::set_var(DEPTH_ENV, MAX_DEPTH.to_string());
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
        std::env::remove_var(DEPTH_ENV);
        assert!(
            out.is_error,
            "spawn should be blocked at max depth: {}",
            out.content
        );
        assert!(
            out.content.contains("depth") || out.content.contains("maximum"),
            "error should mention depth: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn spawn_blocked_beyond_max_depth() {
        let _guard = DEPTH_ENV_MUTEX.lock().await;
        let beyond = MAX_DEPTH + 5;
        std::env::set_var(DEPTH_ENV, beyond.to_string());
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
        std::env::remove_var(DEPTH_ENV);
        assert!(
            out.is_error,
            "spawn should be blocked beyond max depth: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn invalid_action_falls_through_to_spawn() {
        let _guard = DEPTH_ENV_MUTEX.lock().await;
        // An unknown action value defaults to "spawn" behavior.
        std::env::set_var(DEPTH_ENV, MAX_DEPTH.to_string());
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "totally_unknown", "prompt": "x"})))
            .await;
        std::env::remove_var(DEPTH_ENV);
        // At max depth the spawn attempt will be blocked with an error.
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn status_with_empty_string_handle_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "status", "handle": ""})))
            .await;
        assert!(
            out.is_error,
            "empty handle should be an error: {}",
            out.content
        );
    }

    #[tokio::test]
    async fn read_with_inverted_line_range_does_not_panic() {
        let t = make_task();
        let out = t
            .execute(&call(json!({
                "action": "read",
                "handle": "buf_0001",
                "start_line": 9999,
                "end_line": 1
            })))
            .await;
        // Inverted range on a nonexistent handle must not panic.
        let _ = out.is_error;
    }
}
