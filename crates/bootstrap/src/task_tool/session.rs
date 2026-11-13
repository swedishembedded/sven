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
//! The protocol exchange itself ([`drive`]) is written against the ACP
//! `Agent` trait rather than the child process, so it can be exercised
//! without one.
//!
//! Swedish Embedded AB implements solutions for supervising untrusted agent
//! subprocesses over structured protocols for its clients. If your team needs
//! expertise in process supervision and streaming protocol integration, you
//! can procure our services by sending an email to info@swedishembedded.com.
use std::cell::Cell;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::{
    Agent as AcpAgent, CancelNotification, ClientSideConnection, ContentBlock, InitializeRequest,
    NewSessionRequest, PromptRequest, SessionId as AcpSessionId, SessionModeId,
    SessionNotification, SetSessionModeRequest,
};
use futures::StreamExt as _;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use tracing::{debug, warn};

use sven_hsm::ChildRunContract;
use sven_tool_api::{
    events::{SubagentUpdate, ToolEvent},
    tool::ToolOutput,
};
use sven_tools_fs::OutputBufferStore;
use sven_vocab::ApprovalMode;

use super::handback::{transcript_bytes, Child, Reporting};
use super::permission_client::AcpTaskClient;
use super::report::{report, ReportContext, SessionEnd, TurnTokens};
use super::updates::session_update_to_subagent_updates;
use super::{ChildApprover, SUBAGENT_DEPTH_ENV};

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

// ── Arguments passed to the OS thread ────────────────────────────────────────

pub(super) struct SpawnArgs {
    pub(super) exe: PathBuf,
    pub(super) prompt: String,
    /// Images its first message carries, as `(media type, base64)`.
    pub(super) images: Vec<(String, String)>,
    /// The conversation of the sven service its models' outputs belong to:
    /// the parent's.
    pub(super) media_session: Option<String>,
    /// The terms it is held to, written to its stdin before anything else.
    pub(super) parent_terms: Option<super::ParentTerms>,
    pub(super) description: String,
    pub(super) mode: String,
    /// What the child may do, and until when.
    pub(super) contract: ChildRunContract,
    /// Who answers the permission requests the contract does not settle.
    pub(super) approver: Option<ChildApprover>,
    /// How the child's server is held to the parent's terms.
    pub(super) terms: ServeTerms,
    pub(super) workdir: PathBuf,
    pub(super) model_override: Option<String>,
    pub(super) handle_id: String,
    pub(super) call_id: String,
    pub(super) buffer_store: Arc<Mutex<OutputBufferStore>>,
    pub(super) tool_event_tx: mpsc::Sender<ToolEvent>,
    /// Fires when the parent's tool call is cancelled (or `execute` completes)
    /// so `run_acp_session` can forward a `session/cancel` to the child.
    pub(super) cancel_rx: tokio::sync::oneshot::Receiver<()>,
    /// How the child's answer is handed back and accounted.
    pub(super) reporting: Reporting,
}

/// The parent's terms the child's server is started with, beyond the
/// contract's budgets.
#[derive(Clone, Debug, Default)]
pub(super) struct ServeTerms {
    /// Tools the child's server must not offer, as the parent does not.
    pub(super) disabled_tools: Vec<String>,
    /// The parent's approval mode: under manual, every call of the child
    /// that is not read-only comes to the parent.
    pub(super) approval: ApprovalMode,
    /// The parent's `tools.deny_patterns` and `tools.auto_approve_patterns`.
    pub(super) command_patterns: Option<(Vec<String>, Vec<String>)>,
    /// The parent's chunk store, which a child briefed with chunks reads
    /// (read-only) to expand them.
    pub(super) evidence_store: Option<PathBuf>,
}

// ── The protocol exchange ─────────────────────────────────────────────────────

/// Where the child's streamed updates go.
struct UpdateSink {
    tool_event_tx: mpsc::Sender<ToolEvent>,
    call_id: String,
    handle_id: String,
    /// Bytes of the child's transcript seen so far (see `transcript_bytes`).
    transcript: Cell<usize>,
    /// The files the child's tools made or named, by handle (each finished
    /// call's `sven/artifacts`), relayed with the parent's result.
    artifacts: std::cell::RefCell<Vec<sven_vocab::ArtifactRef>>,
}

impl UpdateSink {
    fn count(&self, update: &SubagentUpdate) {
        self.transcript
            .set(self.transcript.get() + transcript_bytes(update));
    }

    /// Forwards one notification, returning the assistant text it carried.
    async fn forward(&self, notification: &SessionNotification) -> Option<String> {
        if let agent_client_protocol::SessionUpdate::ToolCall(call) = &notification.update {
            let made = call
                .meta
                .as_ref()
                .and_then(|meta| meta.get(super::ARTIFACTS_META_KEY))
                .and_then(|value| {
                    serde_json::from_value::<Vec<sven_vocab::ArtifactRef>>(value.clone()).ok()
                });
            if let Some(made) = made {
                let mut artifacts = self.artifacts.borrow_mut();
                for artifact in made {
                    if artifacts.len() < sven_vocab::MAX_ARTIFACTS && !artifacts.contains(&artifact)
                    {
                        artifacts.push(artifact);
                    }
                }
            }
        }
        let (updates, text) = session_update_to_subagent_updates(&notification.update);
        for update in updates {
            self.count(&update);
            let _ = self
                .tool_event_tx
                .send(ToolEvent::SubagentEvent {
                    call_id: self.call_id.clone(),
                    handle_id: self.handle_id.clone(),
                    update,
                })
                .await;
        }
        text
    }
}

/// What [`drive`] needs besides the agent connection.
struct DriveArgs<'a, Exit> {
    workdir: PathBuf,
    mode: String,
    prompt: String,
    /// Images the prompt carries, as `(media type, base64)`.
    images: Vec<(String, String)>,
    /// When the child must be done; `None` for no wall-clock bound.
    deadline: Option<Instant>,
    notifications: futures::channel::mpsc::UnboundedReceiver<SessionNotification>,
    cancel_rx: tokio::sync::oneshot::Receiver<()>,
    /// Resolves when the child process exits.
    exited: Exit,
    updates: &'a UpdateSink,
    /// Longest the child may stay silent, while no permission request of its
    /// is pending.
    inactivity: Duration,
    /// Permission requests of the child being answered right now.
    pending: Rc<Cell<usize>>,
}

/// Runs one prompt against `agent`: handshake, mode switch, then the prompt
/// turn under the inactivity timeout and the deadline. Returns how it ended
/// and the assistant text streamed so far.
///
/// Every end but a completed turn has already sent `session/cancel` where a
/// session existed; killing the child process is the caller's job, and it
/// does so whatever the end.
async fn drive<A, Exit>(agent: Rc<A>, args: DriveArgs<'_, Exit>) -> (SessionEnd, String)
where
    A: AcpAgent + 'static,
    Exit: Future,
    Exit::Output: std::fmt::Debug,
{
    let DriveArgs {
        workdir,
        mode,
        prompt,
        images,
        deadline,
        mut notifications,
        mut cancel_rx,
        exited,
        updates,
        inactivity: silence,
        pending,
    } = args;
    let mut final_text = String::new();

    // NOTE: authenticate is intentionally skipped - the sven ACP server
    // returns an empty authMethods list and the call is not required.
    if let Err(e) = agent
        .initialize(InitializeRequest::new(
            agent_client_protocol::ProtocolVersion::LATEST,
        ))
        .await
    {
        return (
            SessionEnd::Handshake(format!("ACP initialize failed: {e}")),
            final_text,
        );
    }
    let session_id: AcpSessionId = match agent.new_session(NewSessionRequest::new(workdir)).await {
        Ok(r) => r.session_id,
        Err(e) => {
            return (
                SessionEnd::Handshake(format!("ACP new_session failed: {e}")),
                final_text,
            )
        }
    };
    // A child left in its default mode would hold more than the parent
    // allowed it, so a refused switch ends the run.
    if let Err(e) = agent
        .set_session_mode(SetSessionModeRequest::new(
            session_id.clone(),
            SessionModeId::new(mode.as_str()),
        ))
        .await
    {
        agent.cancel(CancelNotification::new(session_id)).await.ok();
        return (SessionEnd::ModeRefused(e.to_string()), final_text);
    }

    let mut blocks = vec![ContentBlock::from(prompt.as_str())];
    blocks.extend(images.into_iter().map(|(mime, data)| {
        ContentBlock::Image(agent_client_protocol::ImageContent::new(data, mime))
    }));
    let prompt_req = PromptRequest::new(session_id.clone(), blocks);
    let prompter = Rc::clone(&agent);
    // Spawned locally so `agent` stays usable for the cancel branches.
    let mut prompt_task =
        tokio::task::spawn_local(async move { prompter.prompt(prompt_req).await });

    // Inactivity timer: reset explicitly on every notification.
    let inactivity = tokio::time::sleep(silence);
    tokio::pin!(inactivity);
    let out_of_time = async {
        match deadline {
            Some(deadline) => tokio::time::sleep_until(deadline).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(out_of_time);
    tokio::pin!(exited);

    let end = loop {
        tokio::select! {
            biased;

            _ = &mut cancel_rx => {
                debug!("task: parent cancelled - forwarding ACP session/cancel");
                break SessionEnd::Cancelled;
            }
            result = &mut prompt_task => {
                break match result {
                    Ok(Ok(resp)) => SessionEnd::Finished(
                        resp.stop_reason,
                        resp.usage.map(|u| TurnTokens {
                            input: u.input_tokens,
                            output: u.output_tokens,
                        }),
                    ),
                    Ok(Err(e)) => {
                        warn!("ACP prompt failed: {e}");
                        SessionEnd::PromptFailed(e.to_string())
                    }
                    Err(e) => SessionEnd::PromptFailed(e.to_string()),
                };
            }
            () = &mut out_of_time => {
                warn!("task: wall-clock budget spent - sending ACP session/cancel");
                break SessionEnd::OutOfTime;
            }
            () = &mut inactivity => {
                if pending.get() > 0 {
                    // Waiting on a person, not on the child.
                    inactivity.as_mut().reset(Instant::now() + silence);
                    continue;
                }
                warn!("task: inactivity timeout - sending ACP session/cancel");
                break SessionEnd::Inactive;
            }
            status = &mut exited => {
                warn!("task: subagent process exited unexpectedly: {status:?}");
                break SessionEnd::ChildGone;
            }
            notification = notifications.next() => match notification {
                Some(notification) => {
                    inactivity.as_mut().reset(Instant::now() + silence);
                    if let Some(text) = updates.forward(&notification).await {
                        final_text.push_str(&text);
                    }
                }
                None => break SessionEnd::ChildGone,
            }
        }
    };
    if !matches!(end, SessionEnd::Finished(..) | SessionEnd::PromptFailed(_)) {
        agent.cancel(CancelNotification::new(session_id)).await.ok();
        prompt_task.abort();
    }

    // Drain any notifications buffered before the loop exited (e.g. queued
    // before PromptResponse arrived or before a child crash was detected).
    // Use try_send: if the receiver is full, drop the update - the final
    // result already captures the accumulated text.
    while let Ok(notification) = notifications.try_recv() {
        let (updates_left, text) = session_update_to_subagent_updates(&notification.update);
        if let Some(text) = text {
            final_text.push_str(&text);
        }
        for update in updates_left {
            updates.count(&update);
            let _ = updates.tool_event_tx.try_send(ToolEvent::SubagentEvent {
                call_id: updates.call_id.clone(),
                handle_id: updates.handle_id.clone(),
                update,
            });
        }
    }
    (end, final_text)
}

// ── Core ACP session logic (runs in LocalSet) ─────────────────────────────────

/// The `sven acp serve` arguments for a child: its model, the tools it must
/// not run, and the contract's budgets and remaining time, which the child's
/// server applies to its own configuration.
///
/// The time is rounded up, so the child's own stop never comes before the
/// parent's deadline: the parent sees the deadline first and reports it. The
/// child waits for permission answers as long as that too - its parent
/// answers them, bounded by the same deadline - or, when the contract has no
/// deadline, for its own default permission timeout: never unbounded.
fn serve_args(
    model_override: Option<&str>,
    contract: &ChildRunContract,
    terms: &ServeTerms,
    now: std::time::Instant,
) -> Vec<String> {
    let mut args = vec!["acp".to_string(), "serve".to_string()];
    let mut flag = |name: &str, value: String| {
        args.push(format!("--{name}"));
        args.push(value);
    };
    if let Some(model) = model_override {
        flag("model", model.to_string());
    }
    if let Some(rounds) = contract.max_tool_rounds {
        flag("max-tool-rounds", rounds.to_string());
    }
    if let Some(tokens) = contract.max_output_tokens {
        flag("max-output-tokens", tokens.to_string());
    }
    for tool in &terms.disabled_tools {
        flag("disable-tool", tool.clone());
    }
    if terms.approval == ApprovalMode::Manual {
        flag("approval", "manual".to_string());
    }
    if let Some((deny, auto_approve)) = &terms.command_patterns {
        let patterns = serde_json::json!({"deny": deny, "auto_approve": auto_approve});
        flag("command-patterns", patterns.to_string());
    }
    if let Some(store) = &terms.evidence_store {
        flag("evidence-store", store.display().to_string());
    }
    let seconds = contract
        .remaining(now)
        .map(|left| (left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1));
    if let Some(seconds) = seconds {
        flag("wall-clock-secs", seconds.to_string());
        flag("permission-timeout-secs", seconds.to_string());
    }
    // A child is as confined as its parent: the same state root and the same
    // egress policy, which are process-wide, never the child's own choice.
    args.extend(confinement_flags(
        sven_roots::state_root(),
        sven_http::egress::installed().map(sven_http::egress::EgressPolicy::config),
    ));
    // A child does not recurse and has no `plan_tasks`: `tools.preload` is
    // the parent's choice of tools, not the child's.
    args.push("--no-preload".to_string());
    args
}

/// The flags of `sven acp serve` that carry a state root and an egress
/// policy to a child (`sven-acp`'s `IsolationFlags` reads them back).
pub fn confinement_flags(
    state_root: Option<&Path>,
    network: Option<&sven_http::egress::NetworkConfig>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(root) = state_root {
        args.extend(["--state-root".to_string(), root.display().to_string()]);
    }
    let Some(network) = network else {
        return args;
    };
    match &network.allow_hosts {
        Some(hosts) if hosts.is_empty() => args.push("--deny-egress".to_string()),
        Some(hosts) => {
            for host in hosts {
                args.extend(["--allow-host".to_string(), host.clone()]);
            }
        }
        None => {}
    }
    if let Some(proxy) = &network.proxy {
        args.extend(["--egress-proxy".to_string(), proxy.clone()]);
    }
    if let Some(bundle) = &network.ca_bundle {
        args.extend(["--ca-bundle".to_string(), bundle.display().to_string()]);
    }
    for (host, addr) in &network.resolve {
        args.extend(["--resolve".to_string(), format!("{host}={addr}")]);
    }
    args
}

pub(super) async fn run_acp_session(args: SpawnArgs, depth: u32) -> ToolOutput {
    let SpawnArgs {
        exe,
        prompt,
        images,
        media_session,
        parent_terms,
        description,
        mode,
        contract,
        approver,
        terms,
        workdir,
        model_override,
        handle_id,
        call_id,
        buffer_store,
        tool_event_tx,
        cancel_rx,
        reporting,
    } = args;

    // ── Spawn child process ───────────────────────────────────────────────────
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(serve_args(
        model_override.as_deref(),
        &contract,
        &terms,
        std::time::Instant::now(),
    ))
    .args(parent_terms.is_some().then_some("--parent-terms-stdin"))
    .env(SUBAGENT_DEPTH_ENV, depth.to_string())
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped()) // capture stderr for diagnostics
    .kill_on_drop(true);

    cmd.current_dir(&workdir);
    // The child goes with the session: killed when this thread, which drives
    // it to its end, ends, even when the whole host is killed.
    sven_tools_exec::confine::die_with_parent(&mut cmd);
    // What the child's models make belongs to this conversation.
    if let Some(session) = &media_session {
        cmd.arg("--media-session").arg(session);
    }

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

    let mut child_stdin = child.stdin.take().expect("stdin piped");
    // A child held to its parent's terms reads them first, before ACP.
    if let Some(terms) = &parent_terms {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_vec(terms).unwrap_or_default();
        line.push(b'\n');
        if let Err(e) = child_stdin.write_all(&line).await {
            let _ = child.kill().await;
            return ToolOutput::err(
                &call_id,
                format!("the sub-agent could not be given its terms: {e}"),
            );
        }
    }
    let child_stdout = child.stdout.take().expect("stdout piped");
    let child_stderr = child.stderr.take().expect("stderr piped");

    // ── Create ACP connection ─────────────────────────────────────────────────
    // Use futures::channel::mpsc (unbounded) since the Client trait methods are
    // !Send and we run inside a LocalSet.
    let (notif_tx, notif_rx) = futures::channel::mpsc::unbounded::<SessionNotification>();

    let deadline = contract
        .remaining(std::time::Instant::now())
        .map(|left| Instant::now() + left);
    let budget_secs = contract
        .remaining(std::time::Instant::now())
        .map_or(0, |left| {
            left.as_secs() + u64::from(left.subsec_nanos() > 0)
        });
    let pending = Rc::new(Cell::new(0));
    let acp_client = AcpTaskClient {
        notification_tx: notif_tx,
        contract,
        approver,
        pending: Rc::clone(&pending),
    };

    let (conn, io_fut) = ClientSideConnection::new(
        acp_client,
        child_stdin.compat_write(),
        child_stdout.compat(),
        |fut| {
            tokio::task::spawn_local(fut);
        },
    );
    let conn = Rc::new(conn);

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

    let updates = UpdateSink {
        tool_event_tx: tool_event_tx.clone(),
        call_id: call_id.clone(),
        handle_id: handle_id.clone(),
        transcript: Cell::new(0),
        artifacts: std::cell::RefCell::default(),
    };
    let (end, final_text) = drive(
        conn,
        DriveArgs {
            workdir,
            mode: mode.clone(),
            prompt,
            images,
            deadline,
            notifications: notif_rx,
            cancel_rx,
            exited: child.wait(),
            updates: &updates,
            inactivity: INACTIVITY_TIMEOUT,
            pending,
        },
    )
    .await;
    // What the child used is charged where the parent's own use is.
    if let SessionEnd::Finished(_, Some(tokens)) = &end {
        let _ = tool_event_tx
            .send(ToolEvent::SubagentEvent {
                call_id: call_id.clone(),
                handle_id: handle_id.clone(),
                update: SubagentUpdate::TokensUsed {
                    input_tokens: tokens.input,
                    output_tokens: tokens.output,
                },
            })
            .await;
    }

    // The ACP server process does not exit on its own after a single prompt
    // turn - kill it now that we have the result, then reap to avoid zombies.
    // Both calls are no-ops if the child has already exited.
    let _ = child.kill().await;
    let _ = child.wait().await;

    // Give an early failure's output a moment to reach stderr.
    let stderr_msg = match end {
        SessionEnd::Handshake(_) | SessionEnd::ModeRefused(_) => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let guard = stderr_buf.lock().unwrap();
            if guard.is_empty() {
                String::new()
            } else {
                format!(
                    "\nChild stderr:\n{}",
                    String::from_utf8_lossy(&guard).trim()
                )
            }
        }
        _ => String::new(),
    };

    // A child that finished is handed back as its finding or as it is, and
    // entered in the parent's account of its children.
    let (handed_back, cost_line) = match &end {
        SessionEnd::Finished(_, tokens) => reporting.conclude(
            &Child {
                handle: &handle_id,
                description: &description,
                used: tokens.map(|t| (t.input, t.output)),
                transcript_bytes: updates.transcript.get(),
            },
            &final_text,
        ),
        _ => (final_text.clone(), None),
    };
    let result = report(
        &end,
        &handed_back,
        &ReportContext {
            mode: &mode,
            budget_secs,
            handle_id: &handle_id,
            description: &description,
            stderr: &stderr_msg,
        },
    );
    match &result {
        Ok(_) => buffer_store.lock().await.finish(&handle_id, 0),
        Err(reason) => buffer_store.lock().await.fail(&handle_id, reason.clone()),
    }

    // Always send Finished so the TUI marks the session done regardless of
    // how the session ended.
    let _ = tool_event_tx
        .send(ToolEvent::SubagentEvent {
            call_id: call_id.clone(),
            handle_id: handle_id.clone(),
            update: SubagentUpdate::Finished { final_text },
        })
        .await;

    match result {
        Ok(text) => {
            let text = match cost_line {
                Some(line) => format!("{text}\n\n{line}"),
                None => text,
            };
            ToolOutput::ok(&call_id, text).with_artifacts(updates.artifacts.take())
        }
        Err(reason) => ToolOutput::err(&call_id, reason),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use agent_client_protocol::StopReason;
    use agent_client_protocol::{
        AuthenticateRequest, AuthenticateResponse, Error as AcpError, InitializeResponse,
        NewSessionResponse, PromptResponse, SetSessionModeResponse, ToolCallUpdate,
        ToolCallUpdateFields,
    };
    use agent_client_protocol::{
        PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
        Result as AcpResult,
    };
    use async_trait::async_trait;
    use sven_hsm::{capability_for_tool_name, ToolCapability};
    use sven_tool_api::tool::ToolCall;
    use sven_tool_api::PermissionRequester;
    use sven_vocab::AgentMode;

    use super::*;
    use crate::task_tool::permission_client::answer_permission;

    fn contract_for(mode: AgentMode) -> ChildRunContract {
        ChildRunContract::new(crate::mode_policy::session_ceiling(mode))
    }

    /// The contract of a child whose parent asks a person about every call
    /// that is not read-only.
    fn manual_contract_for(mode: AgentMode) -> ChildRunContract {
        ChildRunContract::new(crate::mode_policy::session_ceiling(mode).with_manual_approval())
    }

    /// A sub-agent's request for `tool`, naming the capability its kernel
    /// classed the call under, as a sven sub-agent always does.
    fn request(tool: &str) -> RequestPermissionRequest {
        let capability = serde_json::to_value(capability_for_tool_name(tool)).unwrap();
        let mut request = unlabelled(tool);
        request.tool_call.meta = Some(
            [(super::super::CAPABILITY_META_KEY.to_string(), capability)]
                .into_iter()
                .collect(),
        );
        request
    }

    /// A request for `tool` that does not say what the call does.
    fn unlabelled(tool: &str) -> RequestPermissionRequest {
        RequestPermissionRequest::new(
            "s",
            ToolCallUpdate::new(
                "call-1",
                ToolCallUpdateFields::new()
                    .title(tool.to_string())
                    .raw_input(serde_json::json!({"cmd": "ls"})),
            ),
            vec![
                PermissionOption::new("allow", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
            ],
        )
    }

    fn chosen(outcome: &RequestPermissionOutcome) -> String {
        match outcome {
            RequestPermissionOutcome::Selected(selected) => selected.option_id.to_string(),
            other => format!("{other:?}"),
        }
    }

    /// Answers every request the same way and remembers what it was asked.
    struct Approver {
        answer: bool,
        asked: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl PermissionRequester for Approver {
        async fn request_permission(&self, call: &ToolCall, _: ToolCapability) -> bool {
            self.asked.lock().unwrap().push(call.name.clone());
            self.answer
        }
    }

    #[tokio::test]
    async fn a_child_is_allowed_only_what_the_parent_allows_without_asking() {
        let agent = contract_for(AgentMode::Agent);
        let outcome = answer_permission(&agent, None, &request("write_file")).await;
        assert_eq!(chosen(&outcome), "allow");
        let outcome = answer_permission(&agent, None, &request("shell")).await;
        assert_eq!(
            chosen(&outcome),
            "allow",
            "nothing asks unless the policy does"
        );
        let manual = manual_contract_for(AgentMode::Agent);
        let outcome = answer_permission(&manual, None, &request("shell")).await;
        assert_eq!(chosen(&outcome), "reject", "nobody to ask");

        let research = contract_for(AgentMode::Research);
        let outcome = answer_permission(&research, None, &request("write_file")).await;
        assert_eq!(
            chosen(&outcome),
            "reject",
            "a read-only parent's child cannot write"
        );
    }

    fn approver(answer: bool) -> Arc<Approver> {
        Arc::new(Approver {
            answer,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn asked(approver: &Approver) -> Vec<String> {
        approver.asked.lock().unwrap().clone()
    }

    #[tokio::test]
    async fn what_the_contract_does_not_settle_goes_to_the_parent_approver() {
        let agent = manual_contract_for(AgentMode::Agent);
        for answer in [true, false] {
            let gate = approver(answer);
            let child = ChildApprover::Gate(gate.clone());
            let outcome = answer_permission(&agent, Some(&child), &request("shell")).await;
            assert_eq!(chosen(&outcome), if answer { "allow" } else { "reject" });
            assert_eq!(asked(&gate), ["shell"]);
        }
        let gate = approver(true);
        let child = ChildApprover::Gate(gate.clone());
        answer_permission(&agent, Some(&child), &request("read_file")).await;
        assert!(
            asked(&gate).is_empty(),
            "what the parent runs without asking is not put to anyone"
        );
    }

    /// A request that does not name its capability is never taken for a
    /// read on its tool's name alone - an MCP server may call a tool
    /// `read_file` - so the parent's manual approval is asked about it.
    #[tokio::test]
    async fn a_request_without_its_capability_is_not_taken_for_a_read() {
        let agent = manual_contract_for(AgentMode::Agent);
        let gate = approver(true);
        let child = ChildApprover::Gate(gate.clone());
        let outcome = answer_permission(&agent, Some(&child), &unlabelled("read_file")).await;
        assert_eq!(chosen(&outcome), "allow");
        assert_eq!(asked(&gate), ["read_file"]);
        let research = contract_for(AgentMode::Research);
        let outcome = answer_permission(&research, None, &unlabelled("write_file")).await;
        assert_eq!(chosen(&outcome), "reject", "a named write is still a write");
    }

    /// An unknown tool (an MCP one) is held to `NetworkAccess`, as the kernel
    /// holds it: allowed where the contract allows the network, refused
    /// where it does not, whoever could be asked.
    #[tokio::test]
    async fn an_unknown_tool_is_held_to_network_access() {
        let agent = contract_for(AgentMode::Agent);
        let outcome = answer_permission(&agent, None, &request("github-create_issue")).await;
        assert_eq!(chosen(&outcome), "allow");
        let sdlc = contract_for(AgentMode::Sdlc);
        let gate = approver(true);
        let child = ChildApprover::Gate(gate.clone());
        let outcome = answer_permission(&sdlc, Some(&child), &request("github-create_issue")).await;
        assert_eq!(chosen(&outcome), "reject");
        assert!(
            asked(&gate).is_empty(),
            "outside the contract, nobody is asked"
        );
    }

    /// A call beyond the contract is refused even when a person would say
    /// yes, and even when the host asks about every call.
    #[tokio::test]
    async fn a_call_beyond_the_contract_is_refused_without_asking() {
        let research = manual_contract_for(AgentMode::Research);
        for child in [
            ChildApprover::Gate(approver(true)),
            ChildApprover::Host(approver(true)),
        ] {
            let outcome = answer_permission(&research, Some(&child), &request("write_file")).await;
            assert_eq!(chosen(&outcome), "reject");
        }
    }

    #[tokio::test]
    async fn a_parent_whose_host_asks_about_every_call_allows_nothing_outright() {
        let agent = contract_for(AgentMode::Agent);
        let host = approver(false);
        let child = ChildApprover::Host(host.clone());
        let outcome = answer_permission(&agent, Some(&child), &request("write_file")).await;
        assert_eq!(chosen(&outcome), "reject");
        assert_eq!(asked(&host), ["write_file"]);
    }

    #[test]
    fn the_child_server_gets_the_contract_budgets() {
        let now = std::time::Instant::now();
        let contract = contract_for(AgentMode::Agent)
            .with_max_tool_rounds(12)
            .with_max_output_tokens(4096)
            .with_deadline(now + Duration::from_secs(90));
        let terms = ServeTerms {
            disabled_tools: vec!["shell".to_string()],
            approval: ApprovalMode::Manual,
            command_patterns: Some((vec!["rm -rf /*".into()], vec!["ls *".into()])),
            evidence_store: None,
        };
        let args = serve_args(Some("fast"), &contract, &terms, now);
        assert_eq!(
            args,
            [
                "acp",
                "serve",
                "--model",
                "fast",
                "--max-tool-rounds",
                "12",
                "--max-output-tokens",
                "4096",
                "--disable-tool",
                "shell",
                "--approval",
                "manual",
                "--command-patterns",
                r#"{"auto_approve":["ls *"],"deny":["rm -rf /*"]}"#,
                "--wall-clock-secs",
                "90",
                "--permission-timeout-secs",
                "90",
                "--no-preload"
            ],
            "the child gets the parent's budgets, approval mode and command patterns"
        );
        assert_eq!(
            serve_args(
                None,
                &contract_for(AgentMode::Agent),
                &ServeTerms::default(),
                now
            ),
            ["acp", "serve", "--no-preload"],
            "without a deadline the child's own bounded permission timeout applies"
        );
        // A part second rounds up: the child never stops before the parent's
        // own deadline.
        let contract =
            contract_for(AgentMode::Agent).with_deadline(now + Duration::from_millis(89_400));
        let args = serve_args(None, &contract, &ServeTerms::default(), now);
        assert_eq!(args[2..4], ["--wall-clock-secs", "90"]);
    }

    #[test]
    fn a_briefed_child_is_given_the_parents_store_and_nothing_else_is() {
        let store =
            std::path::PathBuf::from("/p/.sven/context/0b9cdb5e-7d0e-4f0e-9d6e-0d3f6a1c2b4a");
        let terms = ServeTerms {
            evidence_store: Some(store.clone()),
            ..ServeTerms::default()
        };
        let args = serve_args(
            None,
            &contract_for(AgentMode::Agent),
            &terms,
            std::time::Instant::now(),
        );
        assert_eq!(
            args,
            [
                "acp",
                "serve",
                "--evidence-store",
                store.to_str().unwrap(),
                "--no-preload"
            ]
        );
    }

    /// The child's kernel names the capability of a call it asks about; the
    /// parent answers by that, not by the tool's name: under manual approval
    /// a web fetch is asked about, a read is not.
    #[tokio::test]
    async fn the_child_named_capability_decides() {
        let manual = manual_contract_for(AgentMode::Agent);
        let with_capability = |tool: &str, capability: ToolCapability| {
            let mut request = request(tool);
            let mut meta = serde_json::Map::new();
            meta.insert(
                super::super::CAPABILITY_META_KEY.to_string(),
                serde_json::to_value(capability).unwrap(),
            );
            request.tool_call.meta = Some(meta);
            request
        };
        let gate = approver(false);
        let child = ChildApprover::Gate(gate.clone());
        let outcome = answer_permission(
            &manual,
            Some(&child),
            &with_capability("memory", ToolCapability::ReadFile),
        )
        .await;
        assert_eq!(chosen(&outcome), "allow");
        let outcome = answer_permission(
            &manual,
            Some(&child),
            &with_capability("web_fetch", ToolCapability::NetworkAccess),
        )
        .await;
        assert_eq!(chosen(&outcome), "reject");
        assert_eq!(asked(&gate), ["web_fetch"]);
    }

    #[tokio::test]
    async fn a_child_shell_request_is_answered_by_the_parent_gate() {
        let agent = manual_contract_for(AgentMode::Agent);
        for approve in [true, false] {
            let (approvals, mut gate) = mpsc::channel(4);
            let approver = ChildApprover::Gate(Arc::new(
                crate::session_handles::GateApprover::new(approvals),
            ));
            let person = tokio::spawn(async move {
                let request: sven_executors::ApprovalRequest =
                    gate.recv().await.expect("the gate is asked");
                assert_eq!(request.capability, sven_hsm::ToolCapability::ExecuteShell);
                assert_eq!(
                    request.call.as_ref().map(|c| c.name.as_str()),
                    Some("shell")
                );
                request.reply_tx.send(approve).unwrap();
            });
            let outcome = answer_permission(&agent, Some(&approver), &request("shell")).await;
            person.await.unwrap();
            assert_eq!(chosen(&outcome), if approve { "allow" } else { "reject" });
        }

        // A gate nobody holds refuses rather than waiting forever.
        let (approvals, gate) = mpsc::channel(4);
        drop(gate);
        let approver = ChildApprover::Gate(Arc::new(crate::session_handles::GateApprover::new(
            approvals,
        )));
        let outcome = answer_permission(&agent, Some(&approver), &request("shell")).await;
        assert_eq!(chosen(&outcome), "reject");
    }

    /// An ACP agent that can refuse the mode switch or never finish a prompt.
    #[derive(Default)]
    struct FakeAgent {
        refuse_mode: bool,
        hang: bool,
        prompted: Cell<bool>,
        cancelled: Cell<bool>,
        /// The media types of the images its prompt carried.
        images: std::cell::RefCell<Vec<String>>,
    }

    #[async_trait(?Send)]
    impl AcpAgent for FakeAgent {
        async fn initialize(&self, _: InitializeRequest) -> AcpResult<InitializeResponse> {
            Ok(InitializeResponse::new(
                agent_client_protocol::ProtocolVersion::LATEST,
            ))
        }
        async fn authenticate(&self, _: AuthenticateRequest) -> AcpResult<AuthenticateResponse> {
            Ok(AuthenticateResponse::new())
        }
        async fn new_session(&self, _: NewSessionRequest) -> AcpResult<NewSessionResponse> {
            Ok(NewSessionResponse::new("s"))
        }
        async fn set_session_mode(
            &self,
            _: SetSessionModeRequest,
        ) -> AcpResult<SetSessionModeResponse> {
            if self.refuse_mode {
                Err(AcpError::invalid_params())
            } else {
                Ok(SetSessionModeResponse::new())
            }
        }
        async fn prompt(&self, request: PromptRequest) -> AcpResult<PromptResponse> {
            self.prompted.set(true);
            for block in request.prompt {
                if let ContentBlock::Image(image) = block {
                    self.images.borrow_mut().push(image.mime_type);
                }
            }
            if self.hang {
                std::future::pending::<()>().await;
            }
            Ok(PromptResponse::new(StopReason::EndTurn))
        }
        async fn cancel(&self, _: CancelNotification) -> AcpResult<()> {
            self.cancelled.set(true);
            Ok(())
        }
    }

    async fn run(agent: Rc<FakeAgent>, deadline: Option<Instant>) -> SessionEnd {
        run_with(
            agent,
            deadline,
            INACTIVITY_TIMEOUT,
            0,
            std::future::pending::<()>(),
        )
        .await
    }

    async fn run_with(
        agent: Rc<FakeAgent>,
        deadline: Option<Instant>,
        inactivity: Duration,
        pending_requests: usize,
        exited: impl Future<Output = ()>,
    ) -> SessionEnd {
        let (tool_event_tx, _events) = mpsc::channel(16);
        let updates = UpdateSink {
            tool_event_tx,
            call_id: "c".into(),
            handle_id: "h".into(),
            transcript: Cell::new(0),
            artifacts: std::cell::RefCell::default(),
        };
        let (_notify, notifications) = futures::channel::mpsc::unbounded();
        let (_cancel, cancel_rx) = tokio::sync::oneshot::channel();
        let local = tokio::task::LocalSet::new();
        let (end, _) = local
            .run_until(drive(
                agent,
                DriveArgs {
                    workdir: PathBuf::from("."),
                    mode: "research".into(),
                    prompt: "look around".into(),
                    images: vec![("image/png".into(), "iVBORw0KGgo=".into())],
                    deadline,
                    notifications,
                    cancel_rx,
                    exited,
                    updates: &updates,
                    inactivity,
                    pending: Rc::new(Cell::new(pending_requests)),
                },
            ))
            .await;
        end
    }

    #[tokio::test]
    async fn a_child_that_refuses_its_mode_is_never_prompted() {
        let agent = Rc::new(FakeAgent {
            refuse_mode: true,
            ..FakeAgent::default()
        });
        let end = run(Rc::clone(&agent), None).await;
        assert!(matches!(end, SessionEnd::ModeRefused(_)), "{end:?}");
        assert!(!agent.prompted.get(), "the child never got the prompt");
    }

    #[tokio::test]
    async fn a_busy_child_is_stopped_at_its_deadline() {
        let agent = Rc::new(FakeAgent {
            hang: true,
            ..FakeAgent::default()
        });
        let deadline = Instant::now() + Duration::from_millis(50);
        let end = tokio::time::timeout(
            Duration::from_secs(10),
            run(Rc::clone(&agent), Some(deadline)),
        )
        .await
        .expect("the deadline ends the session long before the inactivity timeout");
        assert!(matches!(end, SessionEnd::OutOfTime), "{end:?}");
        assert!(agent.cancelled.get(), "the child's session was cancelled");
    }

    #[tokio::test]
    async fn a_child_sees_the_images_it_was_handed_and_its_files_come_back() {
        let agent = Rc::new(FakeAgent::default());
        run(Rc::clone(&agent), None).await;
        assert_eq!(*agent.images.borrow(), ["image/png"]);

        let (tool_event_tx, _events) = mpsc::channel(16);
        let sink = UpdateSink {
            tool_event_tx,
            call_id: "c".into(),
            handle_id: "h".into(),
            transcript: Cell::new(0),
            artifacts: std::cell::RefCell::default(),
        };
        let made = sven_vocab::ArtifactRef {
            uri: "mcp-file://files/7b1d".into(),
            name: "image-1.png".into(),
            mime_type: "image/png".into(),
            size: 9,
        };
        let mut meta = serde_json::Map::new();
        meta.insert(
            super::super::ARTIFACTS_META_KEY.into(),
            serde_json::json!([made]),
        );
        let finished = agent_client_protocol::ToolCall::new("t1", "run_model")
            .status(agent_client_protocol::ToolCallStatus::Completed)
            .meta(meta);
        sink.forward(&SessionNotification::new(
            "s",
            agent_client_protocol::SessionUpdate::ToolCall(finished.clone()),
        ))
        .await;
        // The same call reported again relays its file once.
        sink.forward(&SessionNotification::new(
            "s",
            agent_client_protocol::SessionUpdate::ToolCall(finished),
        ))
        .await;
        assert_eq!(sink.artifacts.take(), [made]);
    }

    #[tokio::test]
    async fn a_child_that_finishes_in_time_reports_its_stop_reason() {
        let agent = Rc::new(FakeAgent::default());
        let end = run(agent, Some(Instant::now() + Duration::from_secs(60))).await;
        assert!(
            matches!(end, SessionEnd::Finished(StopReason::EndTurn, _)),
            "{end:?}"
        );
    }

    #[tokio::test]
    async fn a_pending_permission_request_pauses_the_inactivity_timeout() {
        let silent = || {
            Rc::new(FakeAgent {
                hang: true,
                ..FakeAgent::default()
            })
        };
        let short = Duration::from_millis(30);
        let end = run_with(silent(), None, short, 0, std::future::pending::<()>()).await;
        assert!(matches!(end, SessionEnd::Inactive), "{end:?}");

        let deadline = Instant::now() + Duration::from_millis(300);
        let end = run_with(
            silent(),
            Some(deadline),
            short,
            1,
            std::future::pending::<()>(),
        )
        .await;
        assert!(
            matches!(end, SessionEnd::OutOfTime),
            "a person deciding is not a silent child: {end:?}"
        );
    }

    #[tokio::test]
    async fn a_child_that_exits_mid_turn_is_gone() {
        let agent = Rc::new(FakeAgent {
            hang: true,
            ..FakeAgent::default()
        });
        let end = run_with(agent, None, INACTIVITY_TIMEOUT, 0, async {}).await;
        assert!(matches!(end, SessionEnd::ChildGone), "{end:?}");
    }
}
