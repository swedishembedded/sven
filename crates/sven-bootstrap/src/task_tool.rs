// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TaskTool — spawns a full sven ACP subagent to execute a focused task.
//!
//! # Architecture
//!
//! The task tool spawns `sven acp serve` as a child process and connects to it
//! via the ACP (Agent Client Protocol) over the child's stdin/stdout pipes.
//! This gives fully structured event streaming (text deltas, tool calls, thinking
//! blocks) instead of raw text output.
//!
//! ## Event flow
//!
//! ```text
//! TaskTool                sven acp serve subprocess
//!   │                            │
//!   ├── initialize ─────────────►│
//!   ├── new_session ────────────►│
//!   ├── (set_session_mode) ─────►│
//!   ├── prompt ─────────────────►│
//!   │                            │ ── session/update notifications ──►
//!   │◄── SubagentEvent(TUI) ─────┤    (forwarded to parent TUI)
//!   │                            │
//!   │◄── PromptResponse(done) ───┤
//!   └── ToolOutput(final_text)
//! ```
//!
//! ## Inactivity timeout
//!
//! An atomic timestamp is updated on every ACP notification received.  A
//! `tokio::time::timeout` wraps each channel receive; if no notification arrives
//! within [`INACTIVITY_TIMEOUT`], the child process is killed and the tool returns
//! an error.
//!
//! ## Thread model
//!
//! The ACP `ClientSideConnection` is `!Send` (it uses `LocalBoxFuture` internally
//! for spawning sub-tasks).  To keep the outer `Tool::execute` impl `Send`, the
//! entire ACP session runs in a dedicated `spawn_blocking` thread with its own
//! single-threaded tokio runtime and a `LocalSet`.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    Agent as AcpAgent, CancelNotification, Client, ClientSideConnection, ContentBlock,
    InitializeRequest, NewSessionRequest, PermissionOptionKind, PromptRequest,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    Result as AcpResult, SelectedPermissionOutcome, SessionId as AcpSessionId, SessionModeId,
    SessionNotification, SessionUpdate, SetSessionModeRequest, StopReason, ToolCallStatus,
};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use sven_config::AgentMode;
use sven_tools::{
    events::{SubagentUpdate, ToolEvent},
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
    BufGrepTool, BufReadTool, BufStatusTool, BufferSource, OutputBufferStore,
};

/// Maximum subagent nesting depth (checked via `SVEN_SUBAGENT_DEPTH`).
const MAX_DEPTH: u32 = 3;

/// Environment variable used to track nesting depth across processes.
const DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

/// How long the subagent can be silent before we kill it (10 minutes).
///
/// `AgentEvent::ToolProgress` is forwarded as a heartbeat notification so the
/// timer is reset during long tool calls (builds, shell commands, etc.).  This
/// value is a final safety net for genuinely hung agents.
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);

// ── ACP client handler ────────────────────────────────────────────────────────

/// A minimal ACP `Client` that:
/// - Forwards `session_notification` updates through a channel.
/// - Auto-approves tool permission requests so subagents run unattended.
struct AcpTaskClient {
    notification_tx: futures::channel::mpsc::UnboundedSender<SessionNotification>,
}

#[async_trait(?Send)]
impl Client for AcpTaskClient {
    async fn request_permission(
        &self,
        args: RequestPermissionRequest,
    ) -> AcpResult<RequestPermissionResponse> {
        // Auto-approve: prefer AllowOnce, else first option, else cancelled.
        let chosen_id = args
            .options
            .iter()
            .find(|o| matches!(o.kind, PermissionOptionKind::AllowOnce))
            .or_else(|| args.options.first())
            .map(|o| o.option_id.clone());

        let outcome = if let Some(id) = chosen_id {
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))
        } else {
            RequestPermissionOutcome::Cancelled
        };
        Ok(RequestPermissionResponse::new(outcome))
    }

    async fn session_notification(&self, args: SessionNotification) -> AcpResult<()> {
        let _ = self.notification_tx.unbounded_send(args);
        Ok(())
    }
}

// ── Arguments passed to the blocking thread ───────────────────────────────────

struct SpawnArgs {
    exe: PathBuf,
    prompt: String,
    description: String,
    mode: String,
    workdir: PathBuf,
    model_override: Option<String>,
    handle_id: String,
    call_id: String,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    /// Fires when the parent's tool call is cancelled so `run_acp_session` can
    /// forward a `session/cancel` to the child before killing its process.
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
}

// ── TaskTool ─────────────────────────────────────────────────────────────────

pub struct TaskTool {
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    default_model: Option<String>,
    /// Active cancel senders keyed by `call_id`.  When the parent's tool call
    /// is dropped (cancelled), we fire the sender so the OS thread sends ACP
    /// `session/cancel` to the child before killing it.
    cancel_senders: Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, tokio::sync::oneshot::Sender<()>>>,
    >,
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
            cancel_senders: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
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
         1. Call `task` with prompt → subagent runs and returns its final response\n\
         2. Optionally spawn more sub-agents in parallel with different prompts\n\
         3. The tool blocks until the subagent completes and returns the result\n\n\
         **When to spawn:**\n\
         - Exploring a large unfamiliar area or running a multi-step investigation\n\
         - Implementing a self-contained feature in a specific file/module\n\
         - Running tests, build output, or analyses in parallel\n\n\
         Sub-agents have access to all standard tools. Maximum nesting depth is 3."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
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

        // ── Validate inputs before handing off to the blocking thread ─────────
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
            .unwrap_or("agent")
            .to_string();

        let workdir = call
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));

        let model_override = call
            .args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| self.default_model.clone());

        // Check depth limit.
        let current_depth: u32 = std::env::var(DEPTH_ENV)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if current_depth >= MAX_DEPTH {
            return ToolOutput::err(
                &call.id,
                format!(
                    "maximum sub-agent depth ({MAX_DEPTH}) reached — cannot spawn further sub-agents"
                ),
            );
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("could not locate sven executable: {e}"))
            }
        };

        // Allocate a handle ID for TUI session linking.
        let handle_id = {
            let mut store = self.buffer_store.lock().await;
            store.create(BufferSource::Subagent {
                prompt: prompt.clone(),
                mode: mode.clone(),
                description: description.clone(),
            })
        };

        // Notify TUI to create a child session in the sidebar.
        let _ = self
            .tool_event_tx
            .send(ToolEvent::SubagentStarted {
                call_id: call.id.clone(),
                handle_id: handle_id.clone(),
                description: description.clone(),
                prompt: prompt.clone(),
            })
            .await;

        debug!(
            handle = %handle_id,
            prompt = %prompt,
            mode = %mode,
            depth = current_depth + 1,
            "task: spawning ACP sub-agent"
        );

        // Cancel channel: the receiver travels into the OS thread so it can
        // forward ACP `session/cancel` to the child when the parent is cancelled.
        // The sender is stored here and fired when the parent's task is dropped
        // (via the RAII guard below).
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        {
            let mut senders = self.cancel_senders.lock().await;
            senders.insert(call.id.clone(), cancel_tx);
        }
        let cancel_senders = Arc::clone(&self.cancel_senders);
        let call_id_for_cleanup = call.id.clone();

        let args = SpawnArgs {
            exe,
            prompt,
            description: description.clone(),
            mode,
            workdir,
            model_override,
            handle_id: handle_id.clone(),
            call_id: call.id.clone(),
            buffer_store: Arc::clone(&self.buffer_store),
            tool_event_tx: self.tool_event_tx.clone(),
            cancel_rx,
        };
        let depth_for_env = current_depth + 1;

        // The ACP ClientSideConnection is !Send (uses LocalBoxFuture internally).
        // We run the entire ACP session in a dedicated OS thread (not spawn_blocking,
        // which runs inside the outer tokio thread pool) with its own single-threaded
        // tokio runtime + LocalSet.  Using a plain std::thread avoids any interaction
        // between the outer multi-threaded runtime and the inner single-threaded one.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<ToolOutput>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build sub-agent runtime");
            let local = tokio::task::LocalSet::new();
            let output = rt.block_on(local.run_until(run_acp_session(args, depth_for_env)));
            let _ = result_tx.send(output);
        });
        let result = result_rx
            .await
            .unwrap_or_else(|_| ToolOutput::err(&handle_id, "sub-agent thread died unexpectedly"));

        // Clean up the cancel sender regardless of how the call completed.
        cancel_senders.lock().await.remove(&call_id_for_cleanup);

        result
    }
}

// ── Core ACP session logic (runs in LocalSet) ─────────────────────────────────

async fn run_acp_session(args: SpawnArgs, depth: u32) -> ToolOutput {
    let SpawnArgs {
        exe,
        prompt,
        description,
        mode,
        workdir,
        model_override,
        handle_id,
        call_id,
        buffer_store,
        tool_event_tx,
        cancel_rx,
    } = args;

    // ── Spawn child process ───────────────────────────────────────────────────
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("acp")
        .arg("serve")
        .env(DEPTH_ENV, depth.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()) // capture stderr for diagnostics
        .kill_on_drop(true);

    if let Some(ref m) = model_override {
        cmd.arg("--model").arg(m);
    }

    cmd.current_dir(&workdir);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            buffer_store
                .lock()
                .await
                .fail(&handle_id, format!("failed to spawn: {e}"));
            return ToolOutput::err(&call_id, format!("failed to spawn ACP sub-agent: {e}"));
        }
    };

    if let Some(pid) = child.id() {
        buffer_store.lock().await.set_pid(&handle_id, pid);
    }

    let child_stdin = child.stdin.take().expect("stdin piped");
    let child_stdout = child.stdout.take().expect("stdout piped");
    let child_stderr = child.stderr.take().expect("stderr piped");

    // ── Create ACP connection ─────────────────────────────────────────────────
    // Use futures::channel::mpsc (unbounded) since the Client trait methods are
    // !Send and we run inside a LocalSet.
    let (notif_tx, mut notif_rx) = futures::channel::mpsc::unbounded::<SessionNotification>();

    let acp_client = AcpTaskClient {
        notification_tx: notif_tx,
    };

    // outgoing = writes TO the agent (child stdin)
    // incoming = reads FROM the agent (child stdout)
    //
    // Wrap in Rc so we can share it between the prompt task and the cancel
    // handler below (both live in the same LocalSet / single-threaded runtime).
    let (conn_inner, io_fut) = ClientSideConnection::new(
        acp_client,
        child_stdin.compat_write(),
        child_stdout.compat(),
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );
    let conn = Rc::new(conn_inner);

    // Drain child stderr in a background task so the pipe never fills up and
    // blocks the child.  We collect the last 4 KB for error reporting.
    let stderr_buf: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let stderr_buf = Arc::clone(&stderr_buf);
        tokio::task::spawn_local(async move {
            use tokio::io::AsyncReadExt as _;
            let mut reader = child_stderr;
            let mut chunk = [0u8; 512];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut buf = stderr_buf.lock().unwrap();
                        buf.extend_from_slice(&chunk[..n]);
                        // Keep only the last 4096 bytes.
                        if buf.len() > 4096 {
                            let start = buf.len() - 4096;
                            buf.drain(..start);
                        }
                    }
                }
            }
        });
    }

    tokio::task::spawn_local(async move {
        if let Err(e) = io_fut.await {
            debug!("ACP sub-agent I/O finished: {e}");
        }
    });

    // Helper: read captured stderr and format it for error messages.
    let read_stderr = |stderr_buf: &Arc<std::sync::Mutex<Vec<u8>>>| -> String {
        let buf = stderr_buf.lock().unwrap();
        if buf.is_empty() {
            String::new()
        } else {
            format!("\nChild stderr:\n{}", String::from_utf8_lossy(&buf).trim())
        }
    };

    // ── ACP handshake ─────────────────────────────────────────────────────────
    if let Err(e) = conn
        .initialize(InitializeRequest::new(
            agent_client_protocol::ProtocolVersion::LATEST,
        ))
        .await
    {
        // Give the child a moment to flush any error output to stderr.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let stderr_msg = read_stderr(&stderr_buf);
        let _ = child.kill().await;
        return ToolOutput::err(&call_id, format!("ACP initialize failed: {e}{stderr_msg}"));
    }

    // NOTE: authenticate is intentionally skipped — the sven ACP server
    // returns an empty authMethods list and the call is not required.

    let session_resp = match conn
        .new_session(NewSessionRequest::new(workdir.clone()))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let stderr_msg = read_stderr(&stderr_buf);
            let _ = child.kill().await;
            return ToolOutput::err(&call_id, format!("ACP new_session failed: {e}{stderr_msg}"));
        }
    };
    let acp_session_id: AcpSessionId = session_resp.session_id;

    // Optionally set session mode.
    if mode != "agent" {
        let mode_id = SessionModeId::new(mode.as_str());
        if let Err(e) = conn
            .set_session_mode(SetSessionModeRequest::new(acp_session_id.clone(), mode_id))
            .await
        {
            warn!("ACP set_session_mode failed (non-fatal): {e}");
        }
    }

    // Clone the Rc for use in the prompt task.  Both halves live in the same
    // LocalSet so sharing via Rc is safe (single-threaded cooperative runtime).
    let conn_for_prompt = Rc::clone(&conn);

    // ── Stream prompt with inactivity timeout ─────────────────────────────────

    // Accumulated assistant text and stop reason.
    let mut final_text = String::new();
    let mut timed_out = false;
    let mut stop_reason = StopReason::EndTurn;

    use futures::StreamExt as FuturesStreamExt;

    // Build the prompt request.
    let prompt_content: Vec<ContentBlock> = vec![ContentBlock::from(prompt.as_str())];
    let prompt_req = PromptRequest::new(acp_session_id.clone(), prompt_content);

    // Spawn the prompt as a local task.  We use conn_for_prompt (an Rc clone)
    // rather than moving conn itself so we retain access to conn for cancel below.
    let prompt_task =
        tokio::task::spawn_local(async move { conn_for_prompt.prompt(prompt_req).await });

    // Process notifications with inactivity timeout.
    // Three concurrent branches:
    //   1. Notification arrives from the child (normal streaming).
    //   2. Inactivity timeout — send ACP cancel, then kill the child.
    //   3. Parent-side cancel signal — forward ACP cancel to the child.
    let mut cancel_rx = cancel_rx;
    loop {
        tokio::select! {
            notif_result = tokio::time::timeout(INACTIVITY_TIMEOUT, notif_rx.next()) => {
                match notif_result {
                    Ok(Some(notif)) => {
                        let updates = session_update_to_subagent_updates(&notif.update, &mut final_text);
                        for update in updates {
                            let _ = tool_event_tx
                                .send(ToolEvent::SubagentEvent {
                                    call_id: call_id.clone(),
                                    handle_id: handle_id.clone(),
                                    update,
                                })
                                .await;
                        }
                    }
                    Ok(None) => {
                        // Stream closed — the ACP connection has terminated normally.
                        break;
                    }
                    Err(_inactivity) => {
                        // Gracefully cancel the child before killing it.
                        let cancel_notif = CancelNotification::new(acp_session_id.clone());
                        conn.cancel(cancel_notif).await.ok();
                        timed_out = true;
                        prompt_task.abort();
                        break;
                    }
                }
            }
            _ = &mut cancel_rx => {
                // Parent's tool call was cancelled — propagate to the child.
                debug!(
                    handle = %handle_id,
                    "task: parent cancelled — forwarding ACP session/cancel to child"
                );
                let cancel_notif = CancelNotification::new(acp_session_id.clone());
                conn.cancel(cancel_notif).await.ok();
                stop_reason = StopReason::Cancelled;
                prompt_task.abort();
                break;
            }
        }
    }

    if timed_out {
        let _ = child.kill().await;
        buffer_store.lock().await.fail(
            &handle_id,
            "inactivity timeout after 10 minutes".to_string(),
        );
        return ToolOutput::err(
            &call_id,
            "sub-agent timed out after 10 minutes of inactivity",
        );
    }

    // Drain the prompt task result and extract the stop reason.
    match prompt_task.await {
        Ok(Ok(resp)) => {
            stop_reason = resp.stop_reason;
        }
        Ok(Err(e)) => {
            warn!("ACP prompt error: {e}");
        }
        Err(_aborted) => {
            // Task was aborted (cancel path above already set stop_reason).
        }
    }

    // Clean up child process.
    let exit_code = match child.wait().await {
        Ok(s) => s.code().unwrap_or(-1),
        Err(_) => -1,
    };
    buffer_store.lock().await.finish(&handle_id, exit_code);

    // Send a Finished event so the TUI marks the session done.
    let _ = tool_event_tx
        .send(ToolEvent::SubagentEvent {
            call_id: call_id.clone(),
            handle_id: handle_id.clone(),
            update: SubagentUpdate::Finished {
                final_text: final_text.clone(),
            },
        })
        .await;

    let status_word = if exit_code == 0 { "success" } else { "failed" };
    let stop_word = match stop_reason {
        StopReason::EndTurn => "end_turn",
        StopReason::MaxTokens => "max_tokens",
        StopReason::MaxTurnRequests => "max_turn_requests",
        StopReason::Refusal => "refusal",
        StopReason::Cancelled => "cancelled",
        _ => "unknown",
    };

    if matches!(stop_reason, StopReason::Cancelled) {
        return ToolOutput::err(
            &call_id,
            format!(
                "Sub-agent was cancelled.\n\
                 Handle: {handle_id}\n\
                 Description: {description}"
            ),
        );
    }

    if matches!(stop_reason, StopReason::MaxTokens | StopReason::Refusal) {
        return ToolOutput::err(
            &call_id,
            format!(
                "Sub-agent stopped early: {stop_word} (exit {exit_code}).\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 Partial result:\n{final_text}"
            ),
        );
    }

    if final_text.is_empty() {
        ToolOutput::ok(
            &call_id,
            format!(
                "Sub-agent completed ({status_word}, exit {exit_code}, stop={stop_word}).\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 (No assistant text produced.)"
            ),
        )
    } else {
        ToolOutput::ok(
            &call_id,
            format!(
                "Sub-agent completed ({status_word}, exit {exit_code}, stop={stop_word}).\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 --- Result ---\n{final_text}"
            ),
        )
    }
}

// ── ACP → SubagentUpdate conversion ──────────────────────────────────────────

fn session_update_to_subagent_updates(
    update: &SessionUpdate,
    final_text: &mut String,
) -> Vec<SubagentUpdate> {
    let mut updates = Vec::new();
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => {
                final_text.push_str(&t.text);
                updates.push(SubagentUpdate::TextDelta(t.text.clone()));
            }
            other => {
                debug!(
                    "task: dropping non-text AgentMessageChunk content variant: {:?}",
                    std::mem::discriminant(other)
                );
            }
        },
        SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => {
                updates.push(SubagentUpdate::ThinkingDelta(t.text.clone()));
            }
            other => {
                debug!(
                    "task: dropping non-text AgentThoughtChunk content variant: {:?}",
                    std::mem::discriminant(other)
                );
            }
        },
        SessionUpdate::ToolCall(tc) => {
            let id = tc.tool_call_id.to_string();
            let name = tc.title.clone();
            match tc.status {
                ToolCallStatus::InProgress => {
                    let args = tc.raw_input.clone().unwrap_or(Value::Null);
                    updates.push(SubagentUpdate::ToolCallStarted { id, name, args });
                }
                ToolCallStatus::Completed => {
                    let output = tc
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    updates.push(SubagentUpdate::ToolCallFinished {
                        id,
                        name,
                        output,
                        is_error: false,
                    });
                }
                ToolCallStatus::Failed => {
                    let output = tc
                        .raw_output
                        .as_ref()
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_default();
                    updates.push(SubagentUpdate::ToolCallFinished {
                        id,
                        name,
                        output,
                        is_error: true,
                    });
                }
                _ => {}
            }
        }
        SessionUpdate::Plan(plan) => {
            // Serialize the plan to a text delta so the parent TUI can display
            // the child's todo list without needing a separate SubagentUpdate
            // variant.  Empty plans (heartbeat pings) are silently dropped.
            if !plan.entries.is_empty() {
                let mut text = String::from("[Plan]\n");
                for entry in &plan.entries {
                    let status_icon = match entry.status {
                        agent_client_protocol::PlanEntryStatus::Completed => "✓",
                        agent_client_protocol::PlanEntryStatus::InProgress => "→",
                        _ => "○",
                    };
                    text.push_str(&format!("  {status_icon} {}\n", entry.content));
                }
                updates.push(SubagentUpdate::TextDelta(text));
            }
        }
        SessionUpdate::CurrentModeUpdate(mode_update) => {
            let mode_text = format!("[Mode: {}]\n", mode_update.current_mode_id.0);
            updates.push(SubagentUpdate::TextDelta(mode_text));
        }
        _ => {}
    }
    updates
}

// ── Buffer inspection actions (unchanged) ────────────────────────────────────

impl TaskTool {
    async fn execute_status(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => {
                return ToolOutput::err(
                    &call.id,
                    "missing required parameter 'handle' for action=status",
                )
            }
        };
        let delegate = ToolCall {
            id: call.id.clone(),
            name: "buf_status".into(),
            args: serde_json::json!({ "handle": handle }),
        };
        BufStatusTool::new(self.buffer_store.clone())
            .execute(&delegate)
            .await
    }

    async fn execute_read(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => {
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
        let delegate = ToolCall {
            id: call.id.clone(),
            name: "buf_read".into(),
            args: serde_json::json!({
                "handle": handle,
                "start_line": start_line,
                "end_line": end_line
            }),
        };
        BufReadTool::new(self.buffer_store.clone())
            .execute(&delegate)
            .await
    }

    async fn execute_grep(&self, call: &ToolCall) -> ToolOutput {
        let handle = match call.args.get("handle").and_then(|v| v.as_str()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => {
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
        let delegate = ToolCall {
            id: call.id.clone(),
            name: "buf_grep".into(),
            args: serde_json::json!({
                "handle": handle,
                "pattern": pattern,
                "context_lines": context_lines,
                "limit": limit
            }),
        };
        BufGrepTool::new(self.buffer_store.clone())
            .execute(&delegate)
            .await
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

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
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn read_action_missing_handle_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "read"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn grep_action_missing_handle_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "pattern": "foo"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn grep_action_missing_pattern_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "handle": "buf_0001"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("pattern"));
    }

    #[tokio::test]
    async fn status_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "status", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn read_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "read", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn grep_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({
                "action": "grep",
                "handle": "buf_9999",
                "pattern": "foo"
            })))
            .await;
        assert!(out.is_error);
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
        assert!(out.content.contains("prompt"));
    }

    #[tokio::test]
    async fn spawn_null_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": null}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_integer_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": 42}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_blocked_at_max_depth() {
        let _env = std::env::var(super::DEPTH_ENV).ok();
        std::env::set_var(super::DEPTH_ENV, super::MAX_DEPTH.to_string());
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
        std::env::remove_var(super::DEPTH_ENV);
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
}
