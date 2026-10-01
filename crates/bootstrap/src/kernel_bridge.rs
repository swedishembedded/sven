// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reusable HSM-kernel → [`AgentEvent`] adapter.
//!
//! Every consumer-facing surface (headless CI, interactive TUI, local ACP)
//! consumes an [`AgentEvent`] stream. The kernel ([`crate::RuntimeBuilder`] /
//! [`SessionBundle`]) exposes an outward observation plane of [`UiEvent`]s
//! plus inward [`KernelChannels`] for user-question / approval round-trips.
//!
//! This module bridges the two so any surface consumes the same
//! [`AgentEvent`] contract while running on the kernel. `AgentEvent` and
//! `UiEvent` are both re-exports of the same `sven_vocab::SessionEvent`
//! type, so the "bridge" is just forwarding:
//!
//! * [`spawn_observation_bridge`] — forwards the whole [`UiEvent`] broadcast
//!   into an [`AgentEvent`] `mpsc` channel (streamed text, tool progress,
//!   token usage, mode/model changes, transitions).
//! * [`spawn_question_bridge`] — forwards kernel `AskUser` / approval requests
//!   to a [`QuestionRequest`] channel and relays the answer back (the
//!   tool-approval / clarification round-trip hook).
//! * [`KernelAgentSession`] — a convenience that ties a whole [`SessionBundle`]
//!   together: owns the runtime, spawns both bridges, and exposes `send`,
//!   `cancel`, and the MCP manager. Every interactive surface builds one of
//!   these on top of a [`RuntimeBuilder`](crate::RuntimeBuilder) session.
//!
//! The adapter lives in `sven-bootstrap` (not `sven-frontend`) precisely so the
//! headless `sven-ci` surface can depend on it without pulling
//! in any TUI code, and without creating a dependency cycle — every surface
//! already depends on `sven-bootstrap`.

use std::sync::Arc;

use sven_hsm::UiEvent;
use sven_kernel::ErasedRuntime;
use sven_machines::AgentEvent;
use sven_mcp_client::McpManager;
use sven_tools_agent::{Question, QuestionRequest};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::runtime_builder::{KernelChannels, RuntimeHandle, SessionBundle};

/// Spawn the observation bridge: forward the kernel's outward [`UiEvent`]
/// broadcast into `event_tx` as [`AgentEvent`]s until the observation channel
/// closes (session shutdown).
///
/// A lagging consumer skips events (the inward event queue remains the source
/// of truth); a closed channel ends the task.
pub fn spawn_observation_bridge(
    mut obs_rx: broadcast::Receiver<UiEvent>,
    event_tx: mpsc::Sender<AgentEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match obs_rx.recv().await {
                Ok(ev) => {
                    if event_tx.send(ev).await.is_err() {
                        // Consumer dropped the receiver — nothing left to feed.
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(skipped = n, "kernel bridge: lagged, skipped observations");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!("kernel bridge: observation channel closed, bridge exiting");
                    break;
                }
            }
        }
    })
}

/// Spawn the user-question / approval bridge.
///
/// The kernel's `UserExecutor` forwards `Effect::AskUser` as a
/// [`UserQuestion`](sven_executors::user::UserQuestion) and
/// `Effect::RequestHumanApproval` as an
/// [`ApprovalRequest`](sven_executors::user::ApprovalRequest) on
/// [`KernelChannels`]. This task relays each to `question_tx` as a
/// [`QuestionRequest`] (the same modal channel the TUI already selects on)
/// and sends the user's answer back to the kernel:
///
/// * `AskUser` → free-text question (empty `options`); the raw answer string is
///   returned verbatim, and [`NO_USER_ANSWER`](sven_tool_api::NO_USER_ANSWER)
///   when there is no frontend or it drops the question unanswered.
/// * `RequestHumanApproval` → a yes/no question; `"yes"` (case-insensitive,
///   trimmed) approves, anything else denies. When the modal channel is closed
///   the request is denied: only a session under manual approval asks, and
///   nobody approves on the user's behalf.
///
/// A question or approval the kernel withdraws while it is pending (its run
/// stopped, see `UserExecutor`) is withdrawn from the frontend the same way:
/// the bridge drops its receiver, closing the `QuestionRequest::answer_tx`
/// the frontend holds.
pub fn spawn_question_bridge(
    channels: KernelChannels,
    question_tx: mpsc::Sender<QuestionRequest>,
) -> JoinHandle<()> {
    let KernelChannels {
        mut question_rx,
        mut approval_rx,
    } = channels;
    tokio::spawn(async move {
        loop {
            tokio::select! {
                q = question_rx.recv() => match q {
                    Some(mut kernel_q) => {
                        let (answer_tx, answer_rx) = oneshot::channel::<String>();
                        let req = QuestionRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            questions: vec![Question {
                                prompt: kernel_q.prompt.clone(),
                                options: vec![],
                                allow_multiple: false,
                            }],
                            answer_tx,
                        };
                        if question_tx.send(req).await.is_ok() {
                            // Dropping `answer_rx` when the kernel withdraws
                            // the question withdraws it from the frontend.
                            tokio::select! {
                                answer = answer_rx => {
                                    // Dropped unanswered: nobody will answer.
                                    let answer = answer.unwrap_or_else(|_| {
                                        sven_tool_api::NO_USER_ANSWER.to_string()
                                    });
                                    let _ = kernel_q.reply_tx.send(answer);
                                }
                                () = kernel_q.reply_tx.closed() => {}
                            }
                        } else {
                            // No frontend to show it on: nobody can answer.
                            let _ = kernel_q.reply_tx.send(sven_tool_api::NO_USER_ANSWER.to_string());
                        }
                    }
                    None => break,
                },
                a = approval_rx.recv() => match a {
                    Some(mut approval) => {
                        let mut prompt = format!(
                            "Allow {:?} capability?\n\nAction: {}",
                            approval.capability, approval.description
                        );
                        if let Some(call) = &approval.call {
                            prompt.push_str(&describe_call(call));
                        }
                        let (answer_tx, answer_rx) = oneshot::channel::<String>();
                        let req = QuestionRequest {
                            id: uuid::Uuid::new_v4().to_string(),
                            questions: vec![Question {
                                prompt,
                                options: vec!["yes".to_string(), "no".to_string()],
                                allow_multiple: false,
                            }],
                            answer_tx,
                        };
                        let approved = if question_tx.send(req).await.is_ok() {
                            tokio::select! {
                                answer = answer_rx => answer
                                    .map(|r| r.trim().eq_ignore_ascii_case("yes"))
                                    .unwrap_or(false),
                                // Withdrawn by the kernel: nobody to tell.
                                () = approval.reply_tx.closed() => continue,
                            }
                        } else {
                            false
                        };
                        let _ = approval.reply_tx.send(approved);
                    }
                    None => break,
                },
            }
        }
    })
}

/// What an approval would let run, for the person deciding: the tool and
/// its command (`shell_command` for the shell tool), its path, or (for any
/// other tool) its arguments.
fn describe_call(call: &sven_hsm::GatedCall) -> String {
    let field = |key: &str| call.args.get(key).and_then(serde_json::Value::as_str);
    let command = field("shell_command").or_else(|| field("command"));
    let detail = match (command, field("path")) {
        (Some(command), _) => format!("Command: {command}"),
        (None, Some(path)) => format!("Path: {path}"),
        (None, None) => format!("Arguments: {}", call.args),
    };
    format!("\nTool: {}\n{detail}", call.name)
}

/// A fully-wired kernel session presented as an [`AgentEvent`] stream.
///
/// Owns the [`ErasedRuntime`] (so the kernel stays alive for the session's
/// lifetime), spawns the observation and question bridges, and exposes the
/// hooks a consumer needs: post user input, cancel the in-flight turn, and
/// reach the [`McpManager`] for dynamic-tool / slash-command refresh.
///
/// Dropping the session aborts the kernel and both bridge tasks.
pub struct KernelAgentSession {
    handle: RuntimeHandle,
    mcp_manager: Arc<McpManager>,
    _runtime: ErasedRuntime,
    obs_task: JoinHandle<()>,
    question_task: JoinHandle<()>,
}

impl KernelAgentSession {
    /// Wire a [`SessionBundle`] into an [`AgentEvent`] stream.
    ///
    /// * `event_tx` receives the mapped [`AgentEvent`] stream.
    /// * `question_tx` receives kernel clarification / approval prompts as
    ///   [`QuestionRequest`]s (see [`spawn_question_bridge`]).
    ///
    /// Returns the session alongside the MCP event receiver so the caller can
    /// react to `ToolsChanged` (e.g. refresh slash commands). Observation
    /// subscription happens here, before this returns, so no event emitted
    /// after the first `send` can be missed.
    #[must_use]
    pub fn spawn(
        bundle: SessionBundle,
        event_tx: mpsc::Sender<AgentEvent>,
        question_tx: mpsc::Sender<QuestionRequest>,
    ) -> (Self, mpsc::Receiver<sven_mcp_client::McpEvent>) {
        Self::spawn_with(bundle, event_tx, |channels| {
            spawn_question_bridge(channels, question_tx)
        })
    }

    /// [`Self::spawn`] for a host that answers the session's gates itself:
    /// each question and approval goes to `responder` as a
    /// [`HumanGate`](crate::session_handles::HumanGate), carrying the gated
    /// call and its capability, instead of becoming a modal question.
    #[must_use]
    pub fn spawn_answering(
        bundle: SessionBundle,
        event_tx: mpsc::Sender<AgentEvent>,
        responder: crate::session_handles::HumanGateResponder,
    ) -> (Self, mpsc::Receiver<sven_mcp_client::McpEvent>) {
        Self::spawn_with(bundle, event_tx, |channels| {
            tokio::spawn(channels.forward_to(responder))
        })
    }

    fn spawn_with(
        bundle: SessionBundle,
        event_tx: mpsc::Sender<AgentEvent>,
        answer_gates: impl FnOnce(KernelChannels) -> JoinHandle<()>,
    ) -> (Self, mpsc::Receiver<sven_mcp_client::McpEvent>) {
        let SessionBundle {
            runtime,
            handle,
            channels,
            mcp_manager,
            mcp_event_rx,
        } = bundle;

        let obs_rx = handle.subscribe_observations();
        let obs_task = spawn_observation_bridge(obs_rx, event_tx);
        let question_task = answer_gates(channels);

        let session = Self {
            handle,
            mcp_manager,
            _runtime: runtime,
            obs_task,
            question_task,
        };
        (session, mcp_event_rx)
    }

    /// Post a user message into the kernel. Returns `false` if the kernel queue
    /// is closed.
    pub async fn send_user_message(&self, text: String) -> bool {
        self.handle.send_user_message(text).await
    }

    /// Cancel the in-flight turn (`Event::UserCancelled`). Returns `false` if
    /// the kernel queue is closed.
    pub async fn cancel(&self) -> bool {
        self.handle.cancel().await
    }

    /// A cheap clone of the underlying [`RuntimeHandle`] for callers that need
    /// direct access to the event sink, status watch, or observation stream.
    #[must_use]
    pub fn handle(&self) -> RuntimeHandle {
        self.handle.clone()
    }

    /// The session's [`McpManager`] — query `tools()` after startup and on
    /// `ToolsChanged` to refresh dynamic MCP slash-commands.
    #[must_use]
    pub fn mcp_manager(&self) -> Arc<McpManager> {
        Arc::clone(&self.mcp_manager)
    }

    /// The session's live [`sven_tool_registry::ToolRegistry`] (for MCP tool hot-swap).
    #[must_use]
    pub fn tool_registry(&self) -> Arc<sven_tool_registry::ToolRegistry> {
        self.handle.tool_registry()
    }

    /// Replace the reactive-agent conversation thread with `messages`.
    ///
    /// This is the history-seeding hook the interactive frontends call before
    /// posting a message on the edit-resubmit and resume flows: it installs the
    /// frontend's authoritative reconstructed history so the next turn streams
    /// against exactly those turns rather than the store's own accumulated
    /// version. A no-op if the store mutex is poisoned.
    pub fn seed_history(&self, messages: Vec<sven_model::Message>) {
        if let Ok(mut store) = self.handle.conversation_store().lock() {
            store.replace_thread(
                sven_machines::machines::reactive_agent::CHAT_THREAD,
                messages,
            );
        }
    }

    /// A snapshot of the current reactive-agent conversation thread.
    ///
    /// Used by frontends to carry the accumulated context forward when a
    /// session is rebuilt (e.g. on a mid-session mode/model change), so the
    /// replacement kernel is seeded with the same history.
    #[must_use]
    pub fn history_snapshot(&self) -> Vec<sven_model::Message> {
        self.handle
            .conversation_store()
            .lock()
            .ok()
            .map(|store| store.snapshot(sven_machines::machines::reactive_agent::CHAT_THREAD))
            .unwrap_or_default()
    }

    /// Refresh the live tool registry's MCP tools from the session's
    /// [`McpManager`], so tools that appeared (or vanished) after startup become
    /// usable mid-session without rebuilding the kernel.
    pub async fn refresh_mcp_tools(&self) {
        let mcp_tools = self.mcp_manager.tools().await;
        let tools: Vec<Arc<dyn sven_tool_api::Tool>> = mcp_tools
            .into_iter()
            .map(|t| Arc::new(t) as Arc<dyn sven_tool_api::Tool>)
            .collect();
        self.handle.tool_registry().replace_mcp_tools(tools);
    }
}

impl Drop for KernelAgentSession {
    fn drop(&mut self) {
        // Dropping `_runtime` aborts the kernel consumer (sven-kernel's
        // `AbortOnDrop`); abort the bridge tasks so they don't linger on a
        // closed observation channel.
        self.obs_task.abort();
        self.question_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sven_executors::user::{ApprovalRequest, UserQuestion};
    use sven_hsm::{ApprovalId, ObservationSink, ToolCapability};
    use sven_model_mock::ScriptedMockProvider;
    use sven_tool_api::ToolCall;

    use super::*;
    use crate::runtime_builder::RuntimeBuilder;

    fn tool_args() -> serde_json::Value {
        serde_json::json!({ "command": "ls" })
    }

    /// The spawned observation bridge forwards a live `UiEvent` broadcast into
    /// the `AgentEvent` channel in order, and exits when the sink is dropped.
    /// `AgentEvent`/`UiEvent` are the same type (`sven_vocab::SessionEvent`),
    /// so this is the one remaining thing worth pinning: the forwarding loop
    /// itself, not a translation (see `sven_vocab::SessionEvent`'s doc comment
    /// for why the two names still exist).
    #[tokio::test]
    async fn observation_bridge_forwards_sequence() {
        let sink = ObservationSink::new(32);
        let obs_rx = sink.subscribe();
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(32);
        let task = spawn_observation_bridge(obs_rx, event_tx);

        sink.emit(UiEvent::TextDelta("Hi".into()));
        sink.emit(UiEvent::ToolCallStarted(ToolCall {
            id: "c1".into(),
            name: "shell".into(),
            args: tool_args(),
        }));
        sink.emit(UiEvent::ToolCallFinished {
            call_id: "c1".into(),
            tool_name: "shell".into(),
            output: "ok".into(),
            is_error: false,
        });
        sink.emit(UiEvent::TextComplete("done".into()));
        sink.emit(UiEvent::TurnComplete);
        // Closing the sink ends the bridge loop.
        drop(sink);

        let mut got = Vec::new();
        while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(2), event_rx.recv()).await
        {
            got.push(ev);
        }

        assert_eq!(got.len(), 5, "all five events must be forwarded: {got:?}");
        assert!(matches!(&got[0], AgentEvent::TextDelta(t) if t == "Hi"));
        assert!(matches!(&got[1], AgentEvent::ToolCallStarted(tc) if tc.id == "c1"));
        assert!(matches!(&got[4], AgentEvent::TurnComplete));

        let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
    }

    /// A kernel `AskUser` request is relayed to the `QuestionRequest` channel
    /// and the answer flows back to the kernel side.
    #[tokio::test]
    async fn question_bridge_relays_ask_user() {
        let (kq_tx, question_rx) = mpsc::channel::<UserQuestion>(4);
        let (_ka_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };
        let (ui_tx, mut ui_rx) = mpsc::channel::<QuestionRequest>(4);
        let _task = spawn_question_bridge(channels, ui_tx);

        let (reply_tx, reply_rx) = oneshot::channel::<String>();
        kq_tx
            .send(UserQuestion {
                prompt: "Which file?".into(),
                reply_tx,
            })
            .await
            .unwrap();

        let req = tokio::time::timeout(Duration::from_secs(2), ui_rx.recv())
            .await
            .expect("bridge should forward a QuestionRequest")
            .expect("channel open");
        assert_eq!(req.questions.len(), 1);
        assert_eq!(req.questions[0].prompt, "Which file?");
        assert!(req.questions[0].options.is_empty(), "AskUser is free-text");
        req.answer_tx.send("config.toml".into()).unwrap();

        let answer = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("kernel should get an answer")
            .expect("reply channel open");
        assert_eq!(answer, "config.toml");
    }

    /// A frontend that drops a question without answering it leaves nobody
    /// to answer: the kernel gets the no-user answer, not an empty one.
    #[tokio::test]
    async fn a_question_the_frontend_drops_gets_the_no_user_answer() {
        let (kq_tx, question_rx) = mpsc::channel::<UserQuestion>(4);
        let (_ka_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };
        let (ui_tx, mut ui_rx) = mpsc::channel::<QuestionRequest>(4);
        let _task = spawn_question_bridge(channels, ui_tx);

        let (reply_tx, reply_rx) = oneshot::channel::<String>();
        kq_tx
            .send(UserQuestion {
                prompt: "Which file?".into(),
                reply_tx,
            })
            .await
            .unwrap();
        drop(ui_rx.recv().await.expect("the question is shown"));

        let answer = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("the kernel gets an answer")
            .expect("reply channel open");
        assert_eq!(answer, sven_tool_api::NO_USER_ANSWER);
    }

    /// A question the kernel withdraws (its run stopped, dropping the
    /// reply receiver) is withdrawn from the frontend too, and the bridge
    /// carries on with the next one.
    #[tokio::test]
    async fn question_bridge_withdraws_what_the_kernel_withdraws() {
        let (kq_tx, question_rx) = mpsc::channel::<UserQuestion>(4);
        let (ka_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };
        let (ui_tx, mut ui_rx) = mpsc::channel::<QuestionRequest>(4);
        let _task = spawn_question_bridge(channels, ui_tx);

        let (reply_tx, reply_rx) = oneshot::channel::<String>();
        kq_tx
            .send(UserQuestion {
                prompt: "Which file?".into(),
                reply_tx,
            })
            .await
            .unwrap();
        let mut req = ui_rx.recv().await.expect("forwarded");
        drop(reply_rx);
        tokio::time::timeout(Duration::from_secs(2), req.answer_tx.closed())
            .await
            .expect("the frontend's question is withdrawn");

        let (reply_tx, reply_rx) = oneshot::channel::<bool>();
        ka_tx
            .send(ApprovalRequest {
                approval_id: ApprovalId::new(),
                capability: ToolCapability::ExecuteShell,
                description: "ls".into(),
                call: None,
                reply_tx,
            })
            .await
            .unwrap();
        let mut req = tokio::time::timeout(Duration::from_secs(2), ui_rx.recv())
            .await
            .expect("the next gate is still forwarded")
            .expect("forwarded");
        drop(reply_rx);
        tokio::time::timeout(Duration::from_secs(2), req.answer_tx.closed())
            .await
            .expect("the frontend's approval is withdrawn");
    }

    /// An approval shows what it would let run: the tool, and its command or
    /// path, or its arguments.
    #[tokio::test]
    async fn an_approval_prompt_shows_the_gated_call() {
        let (_kq_tx, question_rx) = mpsc::channel::<UserQuestion>(4);
        let (ka_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };
        let (ui_tx, mut ui_rx) = mpsc::channel::<QuestionRequest>(4);
        let _task = spawn_question_bridge(channels, ui_tx);
        for (name, args, shown) in [
            (
                "shell",
                serde_json::json!({"command": "rm -rf build"}),
                "rm -rf build",
            ),
            (
                "write_file",
                serde_json::json!({"path": "src/lib.rs", "content": "x"}),
                "src/lib.rs",
            ),
            (
                "github-create_issue",
                serde_json::json!({"title": "Bug"}),
                "\"title\":\"Bug\"",
            ),
        ] {
            let (reply_tx, _reply_rx) = oneshot::channel::<bool>();
            ka_tx
                .send(ApprovalRequest {
                    approval_id: ApprovalId::new(),
                    capability: ToolCapability::ExecuteShell,
                    description: "a sub-agent wants to run it".into(),
                    call: Some(sven_hsm::GatedCall {
                        name: name.into(),
                        args,
                    }),
                    reply_tx,
                })
                .await
                .unwrap();
            let req = ui_rx.recv().await.expect("forwarded");
            let prompt = &req.questions[0].prompt;
            assert!(prompt.contains(name) && prompt.contains(shown), "{prompt}");
            req.answer_tx.send("no".into()).unwrap();
        }
    }

    /// A kernel approval request becomes a yes/no `QuestionRequest`; "yes"
    /// approves and the boolean flows back to the kernel.
    #[tokio::test]
    async fn question_bridge_relays_approval() {
        let (_kq_tx, question_rx) = mpsc::channel::<UserQuestion>(4);
        let (ka_tx, approval_rx) = mpsc::channel::<ApprovalRequest>(4);
        let channels = KernelChannels {
            question_rx,
            approval_rx,
        };
        let (ui_tx, mut ui_rx) = mpsc::channel::<QuestionRequest>(4);
        let _task = spawn_question_bridge(channels, ui_tx);

        let (reply_tx, reply_rx) = oneshot::channel::<bool>();
        ka_tx
            .send(ApprovalRequest {
                approval_id: ApprovalId::new(),
                capability: ToolCapability::WriteFile,
                description: "write to src/main.rs".into(),
                call: None,
                reply_tx,
            })
            .await
            .unwrap();

        let req = tokio::time::timeout(Duration::from_secs(2), ui_rx.recv())
            .await
            .expect("bridge should forward an approval QuestionRequest")
            .expect("channel open");
        assert_eq!(
            req.questions[0].options,
            vec!["yes".to_string(), "no".to_string()]
        );
        req.answer_tx.send("yes".into()).unwrap();

        let approved = tokio::time::timeout(Duration::from_secs(2), reply_rx)
            .await
            .expect("kernel should get a decision")
            .expect("reply channel open");
        assert!(approved, "\"yes\" must approve the request");
    }

    /// End-to-end: a real kernel session driven by a mock provider produces a
    /// mappable `AgentEvent` stream through [`KernelAgentSession`].
    #[tokio::test]
    async fn kernel_session_streams_agent_events_with_mock_provider() {
        let mut config = sven_config::Config::default();
        config.model.provider = "mock".into();
        config.model.name = "mock-model".into();

        let bundle = RuntimeBuilder::new(Arc::new(config), "chat")
            .with_model_provider(Box::new(ScriptedMockProvider::always_text("hello there")))
            .build_session()
            .await
            .expect("session should build with the mock provider");

        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(64);
        let (question_tx, _question_rx) = mpsc::channel::<QuestionRequest>(8);
        let (session, _mcp_event_rx) = KernelAgentSession::spawn(bundle, event_tx, question_tx);

        assert!(session.send_user_message("hi".into()).await);

        // Collect the assistant text streamed as AgentEvents until the turn
        // completes (or a generous deadline elapses).
        let mut text = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, event_rx.recv()).await {
                Ok(Some(AgentEvent::TextDelta(d))) => text.push_str(&d),
                Ok(Some(AgentEvent::TextComplete(t))) => {
                    if text.is_empty() {
                        text = t;
                    }
                }
                Ok(Some(AgentEvent::TurnComplete)) => break,
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }

        assert!(
            text.contains("hello there"),
            "streamed assistant text should carry the mock reply, got: {text:?}"
        );
    }
}
