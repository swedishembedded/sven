// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Reusable HSM-kernel → [`AgentEvent`] adapter.
//!
//! Every consumer-facing surface (headless CI, interactive TUI/GUI, P2P node,
//! local ACP) historically drove the legacy `sven_core::Agent` loop and
//! consumed its [`AgentEvent`] stream. The kernel ([`RuntimeBuilder`] /
//! [`SessionBundle`]) instead exposes an outward observation plane of
//! [`UiEvent`]s plus inward [`KernelChannels`] for user-question / approval
//! round-trips.
//!
//! This module bridges the two so any surface can keep consuming the exact
//! same [`AgentEvent`] contract while running on the kernel:
//!
//! * [`ui_event_to_agent_event`] — the pure per-event mapping (single source of
//!   truth; the frontend re-exports it so TUI/GUI never diverge).
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
//! headless `sven-ci` and `sven-node` surfaces can depend on it without pulling
//! in any TUI/GUI code, and without creating a dependency cycle — every surface
//! already depends on `sven-bootstrap`.

use std::sync::Arc;

use sven_core::{AgentEvent, CompactionStrategyUsed, PeerInfo};
use sven_core::prompts::CollabEvent;
use sven_config::AgentMode;
use sven_hsm::{ErasedRuntime, UiEvent};
use sven_mcp_client::McpManager;
use sven_tools::events::{SubagentUpdate, TodoItem};
use sven_tools::{Question, QuestionRequest, ToolCall};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::runtime_builder::{KernelChannels, RuntimeHandle, SessionBundle};

/// Bridge a single [`UiEvent`] from the outward observation plane to the
/// corresponding [`AgentEvent`] expected by existing consumers.
///
/// Returns `None` for observation-only events that have no `AgentEvent`
/// equivalent — currently none, since SDLC transitions are surfaced as a
/// lightweight [`AgentEvent::ToolProgress`] status line — but the `Option`
/// return keeps the mapping total and future-proof.
#[must_use]
pub fn ui_event_to_agent_event(ev: UiEvent) -> Option<AgentEvent> {
    Some(match ev {
        UiEvent::TextDelta(d) => AgentEvent::TextDelta(d),
        UiEvent::TextComplete(t) => AgentEvent::TextComplete(t),
        UiEvent::ThinkingDelta(d) => AgentEvent::ThinkingDelta(d),
        UiEvent::ThinkingComplete(c) => AgentEvent::ThinkingComplete(c),
        UiEvent::ToolStarted {
            call_id,
            name,
            args,
        } => AgentEvent::ToolCallStarted(ToolCall {
            id: call_id,
            name,
            args,
        }),
        UiEvent::ToolProgress { call_id, message } => AgentEvent::ToolProgress { call_id, message },
        UiEvent::ToolFinished {
            call_id,
            name,
            output,
            is_error,
        } => AgentEvent::ToolCallFinished {
            call_id,
            tool_name: name,
            output,
            is_error,
        },
        UiEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            cost_usd,
        } => AgentEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            cost_usd,
        },
        UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => {
            let strategy = match strategy.as_str() {
                "emergency" => CompactionStrategyUsed::Emergency,
                "narrative" => CompactionStrategyUsed::Narrative,
                _ => CompactionStrategyUsed::Structured,
            };
            AgentEvent::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                turn,
            }
        }
        UiEvent::TodoUpdate(v) => {
            let items: Vec<TodoItem> = serde_json::from_value(v).unwrap_or_default();
            AgentEvent::TodoUpdate(items)
        }
        UiEvent::ModeChanged(s) => AgentEvent::ModeChanged(parse_agent_mode(&s)),
        UiEvent::ModelChanged(m) => AgentEvent::ModelChanged(m),
        UiEvent::Error(e) => AgentEvent::Error(e),
        UiEvent::TurnComplete => AgentEvent::TurnComplete,
        UiEvent::Aborted { partial_text } => AgentEvent::Aborted { partial_text },
        // Surface SDLC phase transitions as a lightweight ToolProgress status
        // line (e.g. "SDLC: Planning → GenerateCandidatePlan") so the user sees
        // progress without a chat segment being appended.
        UiEvent::Transition { from, to, event: _ } => AgentEvent::ToolProgress {
            call_id: "sdlc_phase".to_string(),
            message: format!("SDLC: {from} → {to}"),
        },
        // Subagent / delegate / team observations flow back to the exact
        // `AgentEvent`s the TUI consumed before the kernel path existed.
        // Opaque JSON payloads are deserialized back into their typed form;
        // a corrupt payload drops only that one event (`?` → `None`).
        UiEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            prompt,
        } => AgentEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            prompt,
        },
        UiEvent::SubagentEvent {
            call_id,
            handle_id,
            update,
        } => {
            let update: SubagentUpdate = serde_json::from_value(update).ok()?;
            AgentEvent::SubagentEvent {
                call_id,
                handle_id,
                update,
            }
        }
        UiEvent::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        } => AgentEvent::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        },
        UiEvent::CollabEvent(v) => {
            let event: CollabEvent = serde_json::from_value(v).ok()?;
            AgentEvent::CollabEvent(event)
        }
        UiEvent::PeerList(v) => {
            let peers: Vec<PeerInfo> = serde_json::from_value(v).ok()?;
            AgentEvent::PeerList(peers)
        }
    })
}

/// Parse the kernel's mode string label into an [`AgentMode`].
fn parse_agent_mode(s: &str) -> AgentMode {
    match s {
        "chat" | "Chat" => AgentMode::Chat,
        "sdlc" | "Sdlc" => AgentMode::Sdlc,
        "plan" | "Plan" => AgentMode::Plan,
        "research" | "Research" => AgentMode::Research,
        _ => AgentMode::Agent,
    }
}

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
                    if let Some(ae) = ui_event_to_agent_event(ev) {
                        if event_tx.send(ae).await.is_err() {
                            // Consumer dropped the receiver — nothing left to feed.
                            break;
                        }
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
/// [`QuestionRequest`] (the same modal channel the TUI/GUI already select on)
/// and sends the user's answer back to the kernel:
///
/// * `AskUser` → free-text question (empty `options`); the raw answer string is
///   returned verbatim.
/// * `RequestHumanApproval` → a yes/no question; `"yes"` (case-insensitive,
///   trimmed) approves, anything else denies. When the modal channel is closed
///   the request is denied — interactive sessions never blanket auto-approve.
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
                    Some(kernel_q) => {
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
                            if let Ok(answer) = answer_rx.await {
                                let _ = kernel_q.reply_tx.send(answer);
                            } else {
                                let _ = kernel_q.reply_tx.send(String::new());
                            }
                        } else {
                            let _ = kernel_q.reply_tx.send(String::new());
                        }
                    }
                    None => break,
                },
                a = approval_rx.recv() => match a {
                    Some(approval) => {
                        let prompt = format!(
                            "Allow {:?} capability?\n\nAction: {}",
                            approval.capability, approval.description
                        );
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
                            answer_rx
                                .await
                                .map(|r| r.trim().eq_ignore_ascii_case("yes"))
                                .unwrap_or(false)
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
        let SessionBundle {
            runtime,
            handle,
            channels,
            mcp_manager,
            mcp_event_rx,
        } = bundle;

        let obs_rx = handle.subscribe_observations();
        let obs_task = spawn_observation_bridge(obs_rx, event_tx);
        let question_task = spawn_question_bridge(channels, question_tx);

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
}

impl Drop for KernelAgentSession {
    fn drop(&mut self) {
        // Dropping `_runtime` already aborts the kernel consumer; abort the
        // bridge tasks so they don't linger on a closed observation channel.
        self.obs_task.abort();
        self.question_task.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sven_executors::user::{ApprovalRequest, UserQuestion};
    use sven_hsm::{ApprovalId, ObservationSink, ToolCapability};
    use sven_model::ScriptedMockProvider;

    use super::*;
    use crate::runtime_builder::RuntimeBuilder;

    fn tool_args() -> serde_json::Value {
        serde_json::json!({ "command": "ls" })
    }

    /// The pure mapping turns a full turn's worth of `UiEvent`s
    /// (text → tool call → tool result → assistant text → done) into the
    /// matching `AgentEvent` sequence.
    #[test]
    fn maps_full_turn_ui_event_sequence() {
        let seq = vec![
            UiEvent::TextDelta("Hi".into()),
            UiEvent::ToolStarted {
                call_id: "call-1".into(),
                name: "shell".into(),
                args: tool_args(),
            },
            UiEvent::ToolFinished {
                call_id: "call-1".into(),
                name: "shell".into(),
                output: "file.txt".into(),
                is_error: false,
            },
            UiEvent::TextComplete("all done".into()),
            UiEvent::TurnComplete,
        ];

        let mapped: Vec<AgentEvent> = seq
            .into_iter()
            .filter_map(ui_event_to_agent_event)
            .collect();

        assert!(matches!(&mapped[0], AgentEvent::TextDelta(t) if t == "Hi"));
        match &mapped[1] {
            AgentEvent::ToolCallStarted(tc) => {
                assert_eq!(tc.id, "call-1");
                assert_eq!(tc.name, "shell");
                assert_eq!(tc.args, tool_args());
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
        match &mapped[2] {
            AgentEvent::ToolCallFinished {
                call_id,
                tool_name,
                output,
                is_error,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "shell");
                assert_eq!(output, "file.txt");
                assert!(!is_error);
            }
            other => panic!("expected ToolCallFinished, got {other:?}"),
        }
        assert!(matches!(&mapped[3], AgentEvent::TextComplete(t) if t == "all done"));
        assert!(matches!(&mapped[4], AgentEvent::TurnComplete));
        assert_eq!(mapped.len(), 5);
    }

    /// Regression: subagent/delegate/team events must survive the full kernel
    /// path — `AgentEvent → UiEvent` (turn.rs) then `UiEvent → AgentEvent`
    /// (kernel_bridge) — instead of being silently dropped at either boundary.
    /// The TUI's child-session views, delegate summaries, and team collab
    /// segments depend on this round-trip being lossless.
    #[test]
    fn subagent_started_survives_kernel_round_trip() {
        use sven_executors::turn::agent_event_to_ui;

        let original = AgentEvent::SubagentStarted {
            call_id: "call-9".into(),
            handle_id: "buf_0001".into(),
            description: "explore repo".into(),
            prompt: "Find all TODOs".into(),
        };

        let ui = agent_event_to_ui(original)
            .expect("SubagentStarted must map to a UiEvent (was dropped)");
        let back =
            ui_event_to_agent_event(ui).expect("UiEvent must map back to a SubagentStarted");

        match back {
            AgentEvent::SubagentStarted {
                call_id,
                handle_id,
                description,
                prompt,
            } => {
                assert_eq!(call_id, "call-9");
                assert_eq!(handle_id, "buf_0001");
                assert_eq!(description, "explore repo");
                assert_eq!(prompt, "Find all TODOs");
            }
            other => panic!("expected SubagentStarted, got {other:?}"),
        }
    }

    /// Companion to the subagent case: a team `CollabEvent` must also survive
    /// the `AgentEvent → UiEvent → AgentEvent` kernel round-trip intact.
    #[test]
    fn collab_event_survives_kernel_round_trip() {
        use sven_core::prompts::CollabEvent;
        use sven_executors::turn::agent_event_to_ui;

        let original = AgentEvent::CollabEvent(CollabEvent::TeammateSpawned {
            name: "alice".into(),
            role: "reviewer".into(),
        });

        let ui =
            agent_event_to_ui(original).expect("CollabEvent must map to a UiEvent (was dropped)");
        let back = ui_event_to_agent_event(ui).expect("UiEvent must map back to a CollabEvent");

        match back {
            AgentEvent::CollabEvent(CollabEvent::TeammateSpawned { name, role }) => {
                assert_eq!(name, "alice");
                assert_eq!(role, "reviewer");
            }
            other => panic!("expected CollabEvent(TeammateSpawned), got {other:?}"),
        }
    }

    /// The spawned observation bridge forwards a live `UiEvent` broadcast into
    /// the `AgentEvent` channel in order, and exits when the sink is dropped.
    #[tokio::test]
    async fn observation_bridge_forwards_sequence() {
        let sink = ObservationSink::new(32);
        let obs_rx = sink.subscribe();
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(32);
        let task = spawn_observation_bridge(obs_rx, event_tx);

        sink.emit(UiEvent::TextDelta("Hi".into()));
        sink.emit(UiEvent::ToolStarted {
            call_id: "c1".into(),
            name: "shell".into(),
            args: tool_args(),
        });
        sink.emit(UiEvent::ToolFinished {
            call_id: "c1".into(),
            name: "shell".into(),
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
