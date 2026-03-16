// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between sven-frontend async agent events and the Slint UI model.
//!
//! Slint's `ModelRc` is backed by `Rc` and is single-thread (main thread)
//! only.  Background tokio tasks communicate via `Arc<Mutex<Vec<PlainData>>>`
//! and `slint::invoke_from_event_loop` to convert and push updates on the
//! main thread.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use sven_config::{AgentMode, ModelConfig};
use sven_core::AgentEvent;
use sven_frontend::{agent_task, node_agent_task, AgentRequest, NodeBackend, QueuedMessage};
use sven_tools::{OutputBufferStore, QuestionRequest};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::{ChatMessage, MainWindow, SessionItem, ToastItem};

// ── Plain-data structs (Send) for inter-thread communication ─────────────────

#[derive(Clone)]
struct PlainChatMessage {
    message_type: &'static str,
    content: String,
    tool_name: String,
    is_error: bool,
    is_streaming: bool,
    role: &'static str,
}

impl PlainChatMessage {
    fn to_slint(&self) -> ChatMessage {
        ChatMessage {
            message_type: SharedString::from(self.message_type),
            content: SharedString::from(self.content.as_str()),
            tool_name: SharedString::from(self.tool_name.as_str()),
            is_error: self.is_error,
            is_streaming: self.is_streaming,
            role: SharedString::from(self.role),
        }
    }
}

#[derive(Clone)]
struct PlainToast {
    message: String,
    level: &'static str,
}

/// Options for building a `SvenApp`.
pub struct SvenAppOptions {
    pub config: Arc<sven_config::Config>,
    pub model_cfg: ModelConfig,
    pub mode: AgentMode,
    pub node_backend: Option<NodeBackend>,
    pub initial_prompt: Option<String>,
    pub initial_queue: Vec<QueuedMessage>,
}

/// Top-level desktop app handle.
pub struct SvenApp {
    window: MainWindow,
    _agent_tx: mpsc::Sender<AgentRequest>,
}

impl SvenApp {
    /// Build the app (creates window, spawns agent task).
    ///
    /// Must be called on the main thread before the Slint event loop.
    pub async fn build(opts: SvenAppOptions) -> anyhow::Result<Self> {
        let window = MainWindow::new()?;

        // Initial session
        let sessions_model = Rc::new(VecModel::<SessionItem>::default());
        sessions_model.push(SessionItem {
            id: SharedString::from("session-0"),
            title: SharedString::from("New chat"),
            busy: false,
            active: true,
            depth: 0,
            status: SharedString::from("active"),
            current_tool: SharedString::new(),
            total_cost_usd: 0.0,
        });
        window.set_sessions(ModelRc::from(sessions_model.clone()));
        window.set_messages(ModelRc::new(VecModel::<ChatMessage>::default()));
        window.set_toasts(ModelRc::new(VecModel::<ToastItem>::default()));
        window.set_model_name(SharedString::from(opts.config.model.name.clone()));
        window.set_mode(SharedString::from(
            format!("{:?}", opts.mode).to_lowercase(),
        ));

        // ── Agent channels ────────────────────────────────────────────────────
        let (agent_tx, agent_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);
        let (question_tx, _question_rx) = mpsc::channel::<QuestionRequest>(16);
        let cancel_handle: Arc<TokioMutex<Option<tokio::sync::oneshot::Sender<()>>>> =
            Arc::new(TokioMutex::new(None));

        if let Some(ref node) = opts.node_backend {
            let url = node.url.clone();
            let token = node.token.clone();
            let insecure = node.insecure;
            let cancel = Arc::clone(&cancel_handle);
            tokio::spawn(async move {
                node_agent_task(url, token, insecure, agent_rx, event_tx, cancel).await;
            });
        } else {
            let config = Arc::clone(&opts.config);
            let model_cfg = opts.model_cfg.clone();
            let mode = opts.mode;
            let cancel = Arc::clone(&cancel_handle);
            let buf = Arc::new(TokioMutex::new(OutputBufferStore::new()));
            tokio::spawn(async move {
                agent_task(
                    config,
                    model_cfg,
                    mode,
                    agent_rx,
                    event_tx,
                    question_tx,
                    cancel,
                    sven_runtime::SharedSkills::default(),
                    sven_runtime::SharedAgents::default(),
                    sven_tools::SharedTools::default(),
                    sven_tools::SharedToolDisplays::default(),
                    buf,
                    None,
                    None,
                )
                .await;
            });
        }

        // ── Event bridge queues ───────────────────────────────────────────────
        // Declared early so the Slint callbacks below can capture them.
        let pending_msgs: Arc<Mutex<VecDeque<PlainChatMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let pending_toasts: Arc<Mutex<VecDeque<PlainToast>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let streaming_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        // Thinking delta buffer – accumulated while model reasons.
        let thinking_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let pending_title: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let pending_busy: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));
        let pending_metrics: Arc<Mutex<(i32, f32, i32)>> = Arc::new(Mutex::new((0, 0.0, 0)));
        let total_cost = Arc::new(Mutex::new(0.0f64));
        let total_tokens = Arc::new(Mutex::new(0u32));
        let weak = window.as_weak();

        // ── Slint callbacks ───────────────────────────────────────────────────
        let tx = agent_tx.clone();
        let pm_send = Arc::clone(&pending_msgs);
        let weak_send = weak.clone();
        window.on_send_message(move |text| {
            let content = text.to_string();
            if content.is_empty() {
                return;
            }
            // Immediately add the user message so it appears without waiting for the agent.
            pm_send.lock().unwrap().push_back(PlainChatMessage {
                message_type: "user",
                content: content.clone(),
                tool_name: String::new(),
                is_error: false,
                is_streaming: false,
                role: "user",
            });
            let pm2 = Arc::clone(&pm_send);
            let w = weak_send.clone();
            let _ = slint::invoke_from_event_loop(move || flush_messages(pm2, &w));

            let tx = tx.clone();
            tokio::spawn(async move {
                let _ = tx
                    .send(AgentRequest::Submit {
                        content,
                        model_override: None,
                        mode_override: None,
                    })
                    .await;
            });
        });

        let ch = Arc::clone(&cancel_handle);
        window.on_cancel_run(move || {
            let ch = Arc::clone(&ch);
            tokio::spawn(async move {
                if let Some(sender) = ch.lock().await.take() {
                    let _ = sender.send(());
                }
            });
        });

        window.on_new_session(|| {});
        window.on_session_selected(|_| {});
        window.on_question_answered(|_| {});
        window.on_question_dismissed(|| {});

        // ── Event bridge ──────────────────────────────────────────────────────
        let pm = Arc::clone(&pending_msgs);
        let pt = Arc::clone(&pending_toasts);
        let sb = Arc::clone(&streaming_buf);
        let tb = Arc::clone(&thinking_buf);
        let _pti = Arc::clone(&pending_title);
        let pb = Arc::clone(&pending_busy);
        let _pmets = Arc::clone(&pending_metrics);
        let tc = Arc::clone(&total_cost);
        let tt = Arc::clone(&total_tokens);
        let weak2 = weak.clone();

        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgentEvent::TextDelta(delta) => {
                        let mut buf = sb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = buf.clone();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    // First text token: clear any residual thinking stream.
                                    if !win.get_thinking_text().is_empty() {
                                        win.set_thinking_text(SharedString::new());
                                    }
                                    win.set_streaming_text(SharedString::from(text));
                                    win.set_agent_busy(true);
                                }
                            }
                        });
                    }

                    AgentEvent::TextComplete(text) => {
                        *sb.lock().unwrap() = String::new();
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "assistant",
                            content: text,
                            tool_name: String::new(),
                            is_error: false,
                            is_streaming: false,
                            role: "assistant",
                        });
                        *pb.lock().unwrap() = None;
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                flush_messages(pm2, &w);
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_thinking_text(SharedString::new());
                                }
                            }
                        });
                    }

                    // Accumulate thinking deltas into a live ThinkingBubble so the user
                    // sees the model reasoning as it happens rather than waiting for the
                    // full ThinkingComplete event.
                    AgentEvent::ThinkingDelta(delta) => {
                        let mut buf = tb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = buf.clone();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_thinking_text(SharedString::from(text));
                                    win.set_agent_busy(true);
                                }
                            }
                        });
                    }

                    AgentEvent::ThinkingComplete(content) => {
                        // Clear the live thinking stream and commit as a permanent bubble.
                        *tb.lock().unwrap() = String::new();
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "thinking",
                            content,
                            tool_name: String::new(),
                            is_error: false,
                            is_streaming: false,
                            role: "thinking",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                flush_messages(pm2, &w);
                                if let Some(win) = w.upgrade() {
                                    win.set_thinking_text(SharedString::new());
                                }
                            }
                        });
                    }

                    AgentEvent::ToolCallStarted(tc_call) => {
                        let args_preview: String =
                            tc_call.args.to_string().chars().take(200).collect();
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "tool-call",
                            content: args_preview,
                            tool_name: tc_call.name,
                            is_error: false,
                            is_streaming: false,
                            role: "assistant",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                flush_messages(pm2, &w);
                                if let Some(win) = w.upgrade() {
                                    win.set_agent_busy(true);
                                }
                            }
                        });
                    }

                    AgentEvent::ToolCallFinished {
                        output, is_error, ..
                    } => {
                        let preview: String = output.chars().take(500).collect();
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "tool-result",
                            content: preview,
                            tool_name: String::new(),
                            is_error,
                            is_streaming: false,
                            role: "tool",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
                        });
                    }

                    AgentEvent::TurnComplete => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_thinking_text(SharedString::new());
                                    win.set_agent_busy(false);
                                }
                            }
                        });
                    }

                    AgentEvent::Aborted { .. } => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_thinking_text(SharedString::new());
                                    win.set_agent_busy(false);
                                }
                            }
                        });
                    }

                    AgentEvent::Error(err_msg) => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "error",
                            content: err_msg.clone(),
                            tool_name: String::new(),
                            is_error: true,
                            is_streaming: false,
                            role: "error",
                        });
                        pt.lock().unwrap().push_back(PlainToast {
                            message: err_msg,
                            level: "error",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let pt2 = Arc::clone(&pt);
                            let w = weak2.clone();
                            move || {
                                flush_messages(pm2, &w);
                                flush_toasts(pt2, &w);
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_thinking_text(SharedString::new());
                                    win.set_agent_busy(false);
                                }
                            }
                        });
                    }

                    AgentEvent::TokenUsage {
                        input,
                        output,
                        cache_read,
                        cache_write,
                        max_tokens,
                        max_output_tokens,
                        cost_usd,
                        ..
                    } => {
                        let ctx_pct = if max_tokens > 0 {
                            let budget = max_tokens.saturating_sub(max_output_tokens);
                            let prompt = input + cache_read + cache_write;
                            ((prompt as f64 / budget as f64) * 100.0).clamp(0.0, 100.0) as i32
                        } else {
                            0
                        };
                        if output > 0 {
                            let mut t = tt.lock().unwrap();
                            *t = t.saturating_add(output);
                        }
                        if let Some(c) = cost_usd {
                            *tc.lock().unwrap() += c;
                        }
                        let cost_f32 = *tc.lock().unwrap() as f32;
                        let tokens_i32 = *tt.lock().unwrap() as i32;
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_context_pct(ctx_pct);
                                    win.set_total_cost_usd(cost_f32);
                                    win.set_total_output_tokens(tokens_i32);
                                }
                            }
                        });
                    }

                    AgentEvent::TitleGenerated(title) => {
                        let _ = slint::invoke_from_event_loop({
                            let title = title.clone();
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    // Update the sessions model title.
                                    let sessions_model = win.get_sessions();
                                    if sessions_model.row_count() > 0 {
                                        if let Some(mut row) = sessions_model.row_data(0) {
                                            row.title = SharedString::from(title);
                                            sessions_model.set_row_data(0, row);
                                        }
                                    }
                                }
                            }
                        });
                    }

                    AgentEvent::ContextCompacted {
                        tokens_before,
                        tokens_after,
                        strategy,
                        ..
                    } => {
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "system",
                            content: format!(
                                "Context compacted ({strategy}): {tokens_before}→{tokens_after} tokens"
                            ),
                            tool_name: String::new(),
                            is_error: false,
                            is_streaming: false,
                            role: "system",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
                        });
                    }

                    AgentEvent::CollabEvent(ev) => {
                        let text = sven_core::prompts::format_collab_event(&ev);
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "system",
                            content: text,
                            tool_name: String::new(),
                            is_error: false,
                            is_streaming: false,
                            role: "system",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
                        });
                    }

                    AgentEvent::DelegateSummary {
                        to_name,
                        task_title,
                        status,
                        result_preview,
                        ..
                    } => {
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "system",
                            content: format!(
                                "Delegated \"{task_title}\" to {to_name}: {status} — {result_preview}"
                            ),
                            tool_name: String::new(),
                            is_error: false,
                            is_streaming: false,
                            role: "system",
                        });
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
                        });
                    }

                    _ => {}
                }
            }
        });

        // ── Initial prompt / queue ────────────────────────────────────────────
        if let Some(prompt) = opts.initial_prompt {
            let tx = agent_tx.clone();
            tokio::spawn(async move {
                let _ = tx
                    .send(AgentRequest::Submit {
                        content: prompt,
                        model_override: None,
                        mode_override: None,
                    })
                    .await;
            });
        }

        for qmsg in opts.initial_queue {
            let tx = agent_tx.clone();
            tokio::spawn(async move {
                let _ = tx
                    .send(AgentRequest::Submit {
                        content: qmsg.content,
                        model_override: qmsg.model_transition.map(|d| d.into_model_config()),
                        mode_override: qmsg.mode_transition,
                    })
                    .await;
            });
        }

        Ok(Self {
            window,
            _agent_tx: agent_tx,
        })
    }

    /// Run the Slint event loop (blocks until window is closed).
    pub fn run(self) -> anyhow::Result<()> {
        self.window.run()?;
        Ok(())
    }
}

// ── Main-thread flush helpers ──────────────────────────────────────────────────

/// Drain pending chat messages into the window's messages model.
/// Must be called on the Slint main thread.
fn flush_messages(pending: Arc<Mutex<VecDeque<PlainChatMessage>>>, weak: &slint::Weak<MainWindow>) {
    let Some(win) = weak.upgrade() else { return };
    let mut queue = pending.lock().unwrap();
    if queue.is_empty() {
        return;
    }
    let msgs_rc = win.get_messages();
    // Downcast ModelRc to the concrete VecModel so we can call push().
    if let Some(vec_model) = msgs_rc.as_any().downcast_ref::<VecModel<ChatMessage>>() {
        while let Some(plain) = queue.pop_front() {
            vec_model.push(plain.to_slint());
        }
    }
}

/// Drain pending toasts into the window's toasts model.
/// Must be called on the Slint main thread.
fn flush_toasts(pending: Arc<Mutex<VecDeque<PlainToast>>>, weak: &slint::Weak<MainWindow>) {
    let Some(win) = weak.upgrade() else { return };
    let mut queue = pending.lock().unwrap();
    let toasts_rc = win.get_toasts();
    if let Some(vec_model) = toasts_rc.as_any().downcast_ref::<VecModel<ToastItem>>() {
        while let Some(t) = queue.pop_front() {
            vec_model.push(ToastItem {
                message: SharedString::from(t.message),
                level: SharedString::from(t.level),
            });
            // Keep at most 5 toasts
            while vec_model.row_count() > 5 {
                vec_model.remove(0);
            }
        }
    }
}

// Adapter: flush_to_ui used in macro (unused for now, kept for future batch flush)
#[allow(dead_code)]
fn flush_to_ui(
    msgs: Arc<Mutex<VecDeque<PlainChatMessage>>>,
    toasts: Arc<Mutex<VecDeque<PlainToast>>>,
    _streaming: Arc<Mutex<String>>,
    _title: Arc<Mutex<Option<String>>>,
    _busy: Arc<Mutex<Option<bool>>>,
    _metrics: Arc<Mutex<(i32, f32, i32)>>,
    weak: &slint::Weak<MainWindow>,
) {
    flush_messages(msgs, weak);
    flush_toasts(toasts, weak);
}
