// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! The ACP session driver behind [`super::TaskTool`]: spawns `sven acp serve`
//! as a child process, runs the client half of the protocol against it on a
//! dedicated `LocalSet`, and converts the child's `session/update`
//! notifications into [`SubagentUpdate`]s the parent surface can render.
//!
//! Kept apart from `mod.rs` so the tool surface (schema, argument validation,
//! mode resolution) stays readable next to the subprocess/stream plumbing.
//!
//! Swedish Embedded AB implements solutions for supervising untrusted agent
//! subprocesses over structured protocols for its clients. If your team needs
//! expertise in process supervision and streaming protocol integration, you
//! can procure our services by sending an email to info@swedishembedded.com.
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
use futures::StreamExt as _;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use sven_tools::{
    events::{SubagentUpdate, ToolEvent},
    tool::ToolOutput,
};
use sven_tools_fs::OutputBufferStore;

use super::SUBAGENT_DEPTH_ENV;

/// How long the subagent can be silent before we kill it (10 minutes).
///
/// The inactivity timer is reset on every ACP notification, so long-running
/// tool calls (builds, shell commands, etc.) do not trigger a false timeout.
/// This value is a final safety net for genuinely hung agents.
const INACTIVITY_TIMEOUT: Duration = Duration::from_secs(600);

// ── Cancel guard ──────────────────────────────────────────────────────────────

/// RAII guard that fires the cancel sender when dropped.
///
/// Held in `TaskTool::execute`'s stack frame so it fires when `execute`
/// returns normally *or* when the future is dropped mid-flight (parent task
/// cancellation).  The OS thread receives the signal via `cancel_rx` and
/// forwards ACP `session/cancel` to the child before exiting.
pub(super) struct CancelGuard(pub(super) Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

// ── ACP client ────────────────────────────────────────────────────────────────

/// Minimal ACP `Client` for subagent task execution.
///
/// Forwards `session_notification` updates through a channel and auto-approves
/// tool permission requests so subagents run unattended.
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

// ── Arguments passed to the OS thread ────────────────────────────────────────

pub(super) struct SpawnArgs {
    pub(super) exe: PathBuf,
    pub(super) prompt: String,
    pub(super) description: String,
    pub(super) mode: String,
    pub(super) workdir: PathBuf,
    pub(super) model_override: Option<String>,
    pub(super) handle_id: String,
    pub(super) call_id: String,
    pub(super) buffer_store: Arc<Mutex<OutputBufferStore>>,
    pub(super) tool_event_tx: mpsc::Sender<ToolEvent>,
    /// Fires when the parent's tool call is cancelled (or `execute` completes)
    /// so `run_acp_session` can forward a `session/cancel` to the child.
    pub(super) cancel_rx: tokio::sync::oneshot::Receiver<()>,
}

// ── Core ACP session logic (runs in LocalSet) ─────────────────────────────────

pub(super) async fn run_acp_session(args: SpawnArgs, depth: u32) -> ToolOutput {
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
        .env(SUBAGENT_DEPTH_ENV, depth.to_string())
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

    // Wrap conn in Rc so both the prompt task and the cancel handler (both in
    // the same LocalSet) can share it without requiring Send.
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
    let read_stderr = |buf: &Arc<std::sync::Mutex<Vec<u8>>>| -> String {
        let guard = buf.lock().unwrap();
        if guard.is_empty() {
            String::new()
        } else {
            format!(
                "\nChild stderr:\n{}",
                String::from_utf8_lossy(&guard).trim()
            )
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

    // NOTE: authenticate is intentionally skipped - the sven ACP server
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

    // Set session mode.
    let mode_id = SessionModeId::new(mode.as_str());
    if let Err(e) = conn
        .set_session_mode(SetSessionModeRequest::new(acp_session_id.clone(), mode_id))
        .await
    {
        warn!("ACP set_session_mode failed (non-fatal): {e}");
    }

    // Clone the Rc for the prompt task; both halves live in the same LocalSet.
    let conn_for_prompt = Rc::clone(&conn);

    // ── Stream prompt with inactivity timeout ─────────────────────────────────

    let prompt_content: Vec<ContentBlock> = vec![ContentBlock::from(prompt.as_str())];
    let prompt_req = PromptRequest::new(acp_session_id.clone(), prompt_content);

    // Spawn the prompt as a local task so we retain `conn` for the cancel branch.
    let mut prompt_task =
        tokio::task::spawn_local(async move { conn_for_prompt.prompt(prompt_req).await });

    // Inactivity timer: reset explicitly on every notification.
    let inactivity = tokio::time::sleep(INACTIVITY_TIMEOUT);
    tokio::pin!(inactivity);

    let mut final_text = String::new();
    let mut timed_out = false;
    let mut stop_reason = StopReason::EndTurn;
    let mut prompt_error: Option<String> = None;
    let mut cancel_rx = cancel_rx;

    // Five concurrent branches (checked in biased order):
    //   1. Parent cancel signal  - forward ACP session/cancel, stop.
    //   2. PromptResponse        - agent turn complete, stop.
    //   3. Inactivity timeout    - forward ACP session/cancel, stop.
    //   4. Child process exit    - log crash, stop.
    //   5. Notification arrived  - reset timer, forward to TUI, continue.
    loop {
        tokio::select! {
            biased;

            _ = &mut cancel_rx => {
                debug!(handle = %handle_id, "task: parent cancelled - forwarding ACP session/cancel");
                conn.cancel(CancelNotification::new(acp_session_id.clone())).await.ok();
                stop_reason = StopReason::Cancelled;
                prompt_task.abort();
                break;
            }

            result = &mut prompt_task => {
                match result {
                    Ok(Ok(resp)) => {
                        stop_reason = resp.stop_reason;
                    }
                    Ok(Err(e)) => {
                        warn!(handle = %handle_id, "ACP prompt failed: {e}");
                        prompt_error = Some(e.to_string());
                    }
                    Err(_) => {} // aborted via cancel branch
                }
                break;
            }

            _ = &mut inactivity => {
                warn!(handle = %handle_id, "task: inactivity timeout - sending ACP session/cancel");
                conn.cancel(CancelNotification::new(acp_session_id.clone())).await.ok();
                timed_out = true;
                prompt_task.abort();
                break;
            }

            status = child.wait() => {
                warn!(
                    handle = %handle_id,
                    "task: subagent process exited unexpectedly: {:?}", status
                );
                prompt_task.abort();
                break;
            }

            notif = notif_rx.next() => {
                match notif {
                    Some(notif) => {
                        inactivity.as_mut().reset(Instant::now() + INACTIVITY_TIMEOUT);
                        let (updates, text) =
                            session_update_to_subagent_updates(&notif.update);
                        if let Some(t) = text {
                            final_text.push_str(&t);
                        }
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
                    None => break, // notification stream closed (child exited or I/O done)
                }
            }
        }
    }

    // Drain any notifications buffered before the loop exited (e.g. queued
    // before PromptResponse arrived or before a child crash was detected).
    while let Ok(notif) = notif_rx.try_recv() {
        let (updates, text) = session_update_to_subagent_updates(&notif.update);
        if let Some(t) = text {
            final_text.push_str(&t);
        }
        for update in updates {
            // Use try_send: if the receiver is full, drop the update - the
            // final result already captures the accumulated text.
            let _ = tool_event_tx.try_send(ToolEvent::SubagentEvent {
                call_id: call_id.clone(),
                handle_id: handle_id.clone(),
                update,
            });
        }
    }

    // The ACP server process does not exit on its own after a single prompt
    // turn - kill it now that we have the result, then reap to avoid zombies.
    // Both calls are no-ops if the child has already exited.
    let _ = child.kill().await;
    let _ = child.wait().await;

    // ── Consolidated cleanup ──────────────────────────────────────────────────

    let exit_code: i32 = match stop_reason {
        StopReason::EndTurn => 0,
        StopReason::Cancelled => 130, // SIGINT convention
        _ => 1,
    };

    if timed_out {
        buffer_store.lock().await.fail(
            &handle_id,
            "inactivity timeout after 10 minutes".to_string(),
        );
    } else if matches!(stop_reason, StopReason::Cancelled) {
        buffer_store
            .lock()
            .await
            .fail(&handle_id, "cancelled".to_string());
    } else if let Some(ref msg) = prompt_error {
        buffer_store.lock().await.fail(&handle_id, msg.clone());
    } else {
        buffer_store.lock().await.finish(&handle_id, exit_code);
    }

    // Always send Finished so the TUI marks the session done regardless of
    // how the loop exited (success, timeout, cancel, or child crash).
    let _ = tool_event_tx
        .send(ToolEvent::SubagentEvent {
            call_id: call_id.clone(),
            handle_id: handle_id.clone(),
            update: SubagentUpdate::Finished {
                final_text: final_text.clone(),
            },
        })
        .await;

    // ── Build tool output ─────────────────────────────────────────────────────

    if timed_out {
        return ToolOutput::err(
            &call_id,
            "sub-agent timed out after 10 minutes of inactivity",
        );
    }

    if let Some(msg) = prompt_error {
        return ToolOutput::err(
            &call_id,
            format!(
                "sub-agent failed: {msg}\n\
                 Handle: {handle_id}\n\
                 Description: {description}"
            ),
        );
    }

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
                "Sub-agent stopped early: {stop_word}.\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 Partial result:\n{final_text}"
            ),
        );
    }

    let status_word = if exit_code == 0 { "success" } else { "failed" };

    if final_text.is_empty() {
        ToolOutput::ok(
            &call_id,
            format!(
                "Sub-agent completed ({status_word}, stop={stop_word}).\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 (No assistant text produced.)"
            ),
        )
    } else {
        ToolOutput::ok(
            &call_id,
            format!(
                "Sub-agent completed ({status_word}, stop={stop_word}).\n\
                 Handle: {handle_id}\n\
                 Description: {description}\n\n\
                 --- Result ---\n{final_text}"
            ),
        )
    }
}

// ── ACP → SubagentUpdate conversion ──────────────────────────────────────────

/// Convert one ACP [`SessionUpdate`] into zero or more [`SubagentUpdate`]s and
/// an optional text chunk to append to `final_text`.
///
/// The function is pure - callers append the returned text to their accumulator
/// explicitly, keeping side effects visible at the call site.
fn session_update_to_subagent_updates(
    update: &SessionUpdate,
) -> (Vec<SubagentUpdate>, Option<String>) {
    let mut updates = Vec::new();
    let mut text_chunk: Option<String> = None;

    match update {
        SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            ContentBlock::Text(t) => {
                text_chunk = Some(t.text.clone());
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
        SessionUpdate::UsageUpdate(usage) => {
            if let Some(ref cost) = usage.cost {
                updates.push(SubagentUpdate::TokenUsage {
                    cost_usd: cost.amount,
                });
            }
        }
        _ => {}
    }

    (updates, text_chunk)
}
