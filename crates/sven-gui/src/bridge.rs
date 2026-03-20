// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between sven-frontend async agent events and the Slint UI model.
//!
//! `SvenApp::build` sets up the window, registers all callbacks, spawns the
//! agent task, and starts the event-bridge loop.  All heavyweight helpers
//! live in the sibling modules (sessions, queue_ops, search, clipboard, …).

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, FilterModel, Model, ModelRc, SharedString, VecModel};
use sven_config::{AgentMode, ModelConfig};
use sven_core::AgentEvent;
use sven_frontend::commands::completion::fuzzy_score;
use sven_frontend::{
    agent_task,
    commands::{CommandContext, CommandRegistry, ImmediateAction, ParsedCommand},
    node_agent_task,
    queue::QueueState,
    AgentRequest, NodeBackend, QueuedMessage,
};
use sven_input::{chat_path, list_chats, load_chat_from, ChatStatus, ChatUsage, SessionId};
use sven_model::catalog;
use sven_model::{FunctionCall, Message as SvenMessage, MessageContent, Role};
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays, TodoItem};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::{
    clipboard::copy_to_clipboard,
    inspector::{items_from_list, InspectorKind},
    plain_msg::{slint_msg_to_plain, PlainChatMessage, PlainMdBlock, PlainToast},
    queue_ops::sync_queue_model,
    search::new_shared_search,
    sessions::{
        chat_document_to_plain_messages, delete_session_from_disk, format_fields_json,
        markdown_to_md_blocks, markdown_to_plain_messages, save_session_to_disk,
        strip_inline_markdown,
    },
    ChatMessage, CompletionEntry, MainWindow, MdBlock, PickerItem, QuestionItem, QueueItem,
    SessionItem, ToastItem,
};

// ── Thread-local sessions model (main thread only) ─────────────────────────────
std::thread_local! {
    static SESSIONS_MODEL: std::cell::RefCell<Option<Rc<VecModel<SessionItem>>>> =
        const { std::cell::RefCell::new(None) };
}

// ── Format helpers ────────────────────────────────────────────────────────────

fn format_todos_display(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return String::new();
    }
    todos
        .iter()
        .map(|t| format!("{} {}", t.status.icon(), t.content))
        .collect::<Vec<_>>()
        .join("\n")
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Options for building a `SvenApp`.
pub struct SvenAppOptions {
    pub config: Arc<sven_config::Config>,
    pub model_cfg: ModelConfig,
    pub mode: AgentMode,
    pub node_backend: Option<NodeBackend>,
    pub initial_prompt: Option<String>,
    pub initial_queue: Vec<QueuedMessage>,
    pub tool_displays: SharedToolDisplays,
}

/// Top-level desktop app handle.
pub struct SvenApp {
    window: MainWindow,
    _agent_tx: mpsc::Sender<AgentRequest>,
}

impl SvenApp {
    /// Build the app: create window, register callbacks, spawn agent.
    pub async fn build(opts: SvenAppOptions) -> anyhow::Result<Self> {
        let window = MainWindow::new()?;

        // ── Initial session ───────────────────────────────────────────────────
        let initial_session_id = SessionId::new();
        let initial_session_id_str = initial_session_id.as_str().to_string();
        let sessions_model = Rc::new(VecModel::<SessionItem>::default());
        sessions_model.push(SessionItem {
            id: SharedString::from(&initial_session_id_str),
            title: SharedString::from("New chat"),
            busy: false,
            active: true,
            depth: 0,
            status: SharedString::from("active"),
            total_cost_usd: 0.0,
        });

        // Per-session usage map — populated from disk on startup and updated live.
        let session_usage: Arc<Mutex<HashMap<String, ChatUsage>>> =
            Arc::new(Mutex::new(HashMap::new()));

        if let Ok(entries) = list_chats(Some(50)) {
            let mut usage_map = session_usage.lock().unwrap();
            for chat_entry in &entries {
                let id_str = chat_entry.id.as_str().to_string();
                let cost = chat_entry
                    .usage
                    .as_ref()
                    .map(|u| u.total_cost_usd as f32)
                    .unwrap_or(0.0);
                sessions_model.push(SessionItem {
                    id: SharedString::from(&id_str),
                    title: SharedString::from(&chat_entry.title),
                    busy: false,
                    active: false,
                    depth: 0,
                    status: SharedString::from(match chat_entry.status {
                        ChatStatus::Active => "active",
                        ChatStatus::Completed => "completed",
                        ChatStatus::Archived => "archived",
                    }),
                    total_cost_usd: cost,
                });
                if let Some(u) = chat_entry.usage.clone() {
                    usage_map.insert(id_str, u);
                }
            }
        }

        // Shared state (needed early for filter model)
        let session_messages: Arc<Mutex<HashMap<String, Vec<PlainChatMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Wrap sessions in FilterModel for sidebar search; search query is shared state
        let sidebar_search_query: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let sidebar_search_query_for_filter = Arc::clone(&sidebar_search_query);
        let session_msgs_for_filter = Arc::clone(&session_messages);
        let filter_model = Rc::new(FilterModel::new(
            sessions_model.clone(),
            move |session: &SessionItem| {
                let query = sidebar_search_query_for_filter.lock().unwrap();
                if query.is_empty() {
                    return true;
                }
                let q = query.to_lowercase();
                // Fuzzy match on title
                if fuzzy_score(&q, session.title.as_str()).is_some() {
                    return true;
                }
                // Match status
                if session.status.to_lowercase().contains(&q) {
                    return true;
                }
                // Match chat content for loaded sessions (cached in memory)
                if let Some(msgs) = session_msgs_for_filter
                    .lock()
                    .unwrap()
                    .get(session.id.as_str())
                {
                    let content: String = msgs
                        .iter()
                        .map(|m| m.content.to_lowercase())
                        .collect::<Vec<_>>()
                        .join(" ");
                    if content.contains(&q) {
                        return true;
                    }
                    for m in msgs {
                        if fuzzy_score(&q, &m.content).is_some() {
                            return true;
                        }
                    }
                }
                false
            },
        ));
        window.set_sessions(ModelRc::from(filter_model.clone()));
        window.set_active_session_id(SharedString::from(&initial_session_id_str));

        SESSIONS_MODEL.with(|tl| *tl.borrow_mut() = Some(Rc::clone(&sessions_model)));

        let msgs_model = Rc::new(VecModel::<ChatMessage>::default());
        let streaming_msgs_model = Rc::new(VecModel::<ChatMessage>::default());
        window.set_messages(ModelRc::from(msgs_model.clone()));
        window.set_streaming_messages(ModelRc::from(streaming_msgs_model.clone()));
        window.set_toasts(ModelRc::new(VecModel::<ToastItem>::default()));
        let queue_items_model = Rc::new(VecModel::<QueueItem>::default());
        window.set_queue_items(ModelRc::from(queue_items_model.clone()));
        window.set_model_name(SharedString::from(&opts.model_cfg.name));
        window.set_mode(SharedString::from(
            format!("{:?}", opts.mode).to_lowercase(),
        ));

        // ── Shared state ──────────────────────────────────────────────────────
        let active_session_id: Arc<Mutex<String>> =
            Arc::new(Mutex::new(initial_session_id_str.clone()));
        let current_mode = Arc::new(Mutex::new(opts.mode));
        let current_model_name = Arc::new(Mutex::new(opts.model_cfg.name.clone()));
        let current_model_provider = Arc::new(Mutex::new(opts.model_cfg.provider.clone()));

        // ── Agent channels ────────────────────────────────────────────────────
        let (agent_tx, agent_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);
        let (question_tx, mut question_rx) = mpsc::channel::<QuestionRequest>(16);
        let cancel_handle: Arc<TokioMutex<Option<tokio::sync::oneshot::Sender<()>>>> =
            Arc::new(TokioMutex::new(None));

        let tool_displays = opts.tool_displays.clone();

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
            let td = tool_displays.clone();
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
                    td,
                    buf,
                    None,
                    None,
                )
                .await;
            });
        }

        // ── Streaming buffers ─────────────────────────────────────────────────
        let pending_msgs: Arc<Mutex<VecDeque<PlainChatMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let pending_toasts: Arc<Mutex<VecDeque<PlainToast>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let streaming_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let thinking_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

        let queue_state: Arc<Mutex<QueueState>> = Arc::new(Mutex::new(QueueState::new()));
        let editing_msg_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let is_first_message: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        let streaming_session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let current_todos: Arc<Mutex<HashMap<String, Vec<TodoItem>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Input history per session
        let input_history: Arc<Mutex<VecDeque<String>>> =
            Arc::new(Mutex::new(VecDeque::with_capacity(100)));
        let input_history_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));

        // Search state
        let search = new_shared_search();

        let weak = window.as_weak();

        // ── Message edit ──────────────────────────────────────────────────────
        {
            let msgs_edit = Rc::clone(&msgs_model);
            let editing = Arc::clone(&editing_msg_index);
            let weak_edit = weak.clone();
            window.on_message_edit_requested(move |idx| {
                let idx = idx as usize;
                if let Some(m) = msgs_edit.row_data(idx) {
                    if m.message_type.as_str() == "user" {
                        *editing.lock().unwrap() = Some(idx);
                        if let Some(win) = weak_edit.upgrade() {
                            win.set_input_text(m.content.clone());
                        }
                    }
                }
            });
        }

        // ── Toggle expand ─────────────────────────────────────────────────────
        {
            let msgs_clone = Rc::clone(&msgs_model);
            window.on_toggle_expanded(move |idx| {
                let idx = idx as usize;
                if let Some(mut row) = msgs_clone.row_data(idx) {
                    row.is_expanded = !row.is_expanded;
                    msgs_clone.set_row_data(idx, row);
                }
            });
        }

        // ── Copy text ─────────────────────────────────────────────────────────
        {
            window.on_copy_text(move |text| {
                copy_to_clipboard(text.as_str());
            });
        }

        // ── Input history ─────────────────────────────────────────────────────
        {
            let hist = Arc::clone(&input_history);
            let hist_idx = Arc::clone(&input_history_index);
            let weak_h = weak.clone();
            window.on_input_history_prev(move || {
                let h = hist.lock().unwrap();
                if h.is_empty() {
                    return;
                }
                let mut idx = hist_idx.lock().unwrap();
                let new_idx = match *idx {
                    None => 0,
                    Some(i) => (i + 1).min(h.len() - 1),
                };
                *idx = Some(new_idx);
                if let Some(entry) = h.get(new_idx) {
                    if let Some(win) = weak_h.upgrade() {
                        win.set_input_text(SharedString::from(entry.as_str()));
                    }
                }
            });

            let hist2 = Arc::clone(&input_history);
            let hist_idx2 = Arc::clone(&input_history_index);
            let weak_h2 = weak.clone();
            window.on_input_history_next(move || {
                let mut idx = hist_idx2.lock().unwrap();
                let new_idx = match *idx {
                    None | Some(0) => {
                        *idx = None;
                        if let Some(win) = weak_h2.upgrade() {
                            win.set_input_text(SharedString::new());
                        }
                        return;
                    }
                    Some(i) => i - 1,
                };
                *idx = Some(new_idx);
                let h = hist2.lock().unwrap();
                if let Some(entry) = h.get(new_idx) {
                    if let Some(win) = weak_h2.upgrade() {
                        win.set_input_text(SharedString::from(entry.as_str()));
                    }
                }
            });
        }

        // ── Search ────────────────────────────────────────────────────────────
        {
            let search_q = Arc::clone(&search);
            let msgs_s = Rc::clone(&msgs_model);
            let weak_s = weak.clone();
            window.on_search_query_changed(move |query| {
                let contents: Vec<String> = (0..msgs_s.row_count())
                    .filter_map(|i| msgs_s.row_data(i))
                    .map(|m| m.content.to_string())
                    .collect();
                let mut ss = search_q.lock().unwrap();
                ss.update(query.as_str(), &contents);
                let match_count = ss.match_count() as i32;
                let current = ss.current as i32;
                let match_row = ss.current_row().map(|r| r as i32).unwrap_or(-1);
                drop(ss);
                // Update search highlight on messages
                for i in 0..msgs_s.row_count() {
                    if let Some(mut row) = msgs_s.row_data(i) {
                        row.is_search_match = false;
                        msgs_s.set_row_data(i, row);
                    }
                }
                if match_row >= 0 {
                    if let Some(mut row) = msgs_s.row_data(match_row as usize) {
                        row.is_search_match = true;
                        msgs_s.set_row_data(match_row as usize, row);
                    }
                }
                if let Some(win) = weak_s.upgrade() {
                    win.set_search_match_count(match_count);
                    win.set_search_current_match(current);
                }
            });

            let search_n = Arc::clone(&search);
            let msgs_sn = Rc::clone(&msgs_model);
            let weak_sn = weak.clone();
            window.on_search_next(move || {
                let mut ss = search_n.lock().unwrap();
                ss.next();
                let current = ss.current as i32;
                let match_row = ss.current_row().map(|r| r as i32).unwrap_or(-1);
                drop(ss);
                clear_search_highlights(&msgs_sn);
                if match_row >= 0 {
                    if let Some(mut row) = msgs_sn.row_data(match_row as usize) {
                        row.is_search_match = true;
                        msgs_sn.set_row_data(match_row as usize, row);
                    }
                }
                if let Some(win) = weak_sn.upgrade() {
                    win.set_search_current_match(current);
                }
            });

            let search_p = Arc::clone(&search);
            let msgs_sp = Rc::clone(&msgs_model);
            let weak_sp = weak.clone();
            window.on_search_prev(move || {
                let mut ss = search_p.lock().unwrap();
                ss.prev();
                let current = ss.current as i32;
                let match_row = ss.current_row().map(|r| r as i32).unwrap_or(-1);
                drop(ss);
                clear_search_highlights(&msgs_sp);
                if match_row >= 0 {
                    if let Some(mut row) = msgs_sp.row_data(match_row as usize) {
                        row.is_search_match = true;
                        msgs_sp.set_row_data(match_row as usize, row);
                    }
                }
                if let Some(win) = weak_sp.upgrade() {
                    win.set_search_current_match(current);
                }
            });
        }

        // ── Send message ──────────────────────────────────────────────────────
        {
            let tx = agent_tx.clone();
            let tx2 = agent_tx.clone();
            let pm_send = Arc::clone(&pending_msgs);
            let weak_send = weak.clone();
            let is_first = Arc::clone(&is_first_message);
            let queue = Arc::clone(&queue_state);
            let queue_items_send = Rc::clone(&queue_items_model);
            let editing_send = Arc::clone(&editing_msg_index);
            let cur_mode = Arc::clone(&current_mode);
            let cur_provider = Arc::clone(&current_model_provider);
            let cur_model = Arc::clone(&current_model_name);
            let config = Arc::clone(&opts.config);
            let sb_send = Arc::clone(&streaming_buf);
            let tb_send = Arc::clone(&thinking_buf);
            let cancel_handle_sm = Arc::clone(&cancel_handle);
            let streaming_sid = Arc::clone(&streaming_session_id);
            let active_sid_send = Arc::clone(&active_session_id);
            let session_msgs_send = Arc::clone(&session_messages);
            let pt_send = Arc::clone(&pending_toasts);
            let msgs_send = Rc::clone(&msgs_model);
            let hist_send = Arc::clone(&input_history);
            let hist_idx_send = Arc::clone(&input_history_index);

            window.on_send_message(move |text| {
                let content = text.to_string();
                if content.is_empty() {
                    return;
                }

                // Save to input history
                {
                    let mut h = hist_send.lock().unwrap();
                    if h.front() != Some(&content) {
                        h.push_front(content.clone());
                        if h.len() > 200 {
                            h.pop_back();
                        }
                    }
                    *hist_idx_send.lock().unwrap() = None;
                }

                // Edit-and-restart
                if let Some(idx) = editing_send.lock().unwrap().take() {
                    let mut plain: Vec<PlainChatMessage> = (0..idx)
                        .filter_map(|i| msgs_send.row_data(i))
                        .map(|m| slint_msg_to_plain(&m))
                        .collect();
                    let new_user_msgs = markdown_to_plain_messages(&content, "user");
                    plain.extend(new_user_msgs.clone());
                    let messages = plain_messages_to_sven_messages(&plain);
                    let sid = active_sid_send.lock().unwrap().clone();
                    while msgs_send.row_count() > idx {
                        msgs_send.remove(idx);
                    }
                    for (i, m) in new_user_msgs.iter().enumerate() {
                        msgs_send.insert(idx + i, m.to_slint());
                    }
                    session_msgs_send
                        .lock()
                        .unwrap()
                        .insert(sid.clone(), plain.clone());
                    *sb_send.lock().unwrap() = String::new();
                    *tb_send.lock().unwrap() = String::new();
                    *streaming_sid.lock().unwrap() = Some(sid.clone());
                    let mode_val = *cur_mode.lock().unwrap();
                    if let Some(win) = weak_send.upgrade() {
                        win.set_streaming_text(SharedString::new());
                        win.set_streaming_messages(
                            ModelRc::new(VecModel::<ChatMessage>::default()),
                        );
                        win.set_thinking_text(SharedString::new());
                        win.set_agent_busy(true);
                    }
                    let tx_resubmit = tx.clone();
                    tokio::spawn(async move {
                        let _ = tx_resubmit
                            .send(AgentRequest::Resubmit {
                                messages,
                                new_user_content: content,
                                model_override: None,
                                mode_override: Some(mode_val),
                            })
                            .await;
                    });
                    if let Some(win) = weak_send.upgrade() {
                        win.set_input_text(SharedString::new());
                    }
                    return;
                }

                let agent_busy = weak_send
                    .upgrade()
                    .map(|w| w.get_agent_busy())
                    .unwrap_or(false);

                // Slash command dispatch
                if content.starts_with('/') {
                    let registry = CommandRegistry::with_builtins();
                    let ctx = CommandContext {
                        config: Arc::clone(&config),
                        current_model_provider: cur_provider.lock().unwrap().clone(),
                        current_model_name: cur_model.lock().unwrap().clone(),
                    };
                    if let Some((_, result)) =
                        sven_frontend::commands::dispatch_command(&content, &registry, &ctx)
                    {
                        if let Some(action) = result.immediate_action {
                            let w = weak_send.clone();
                            let pm3 = Arc::clone(&pm_send);
                            let sb3 = Arc::clone(&sb_send);
                            let tb3 = Arc::clone(&tb_send);
                            let ch3 = Arc::clone(&cancel_handle_sm);
                            let _pt3 = Arc::clone(&pt_send);
                            match action {
                                ImmediateAction::ClearChat => {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        *sb3.lock().unwrap() = String::new();
                                        *tb3.lock().unwrap() = String::new();
                                        pm3.lock().unwrap().clear();
                                        if let Some(win) = w.upgrade() {
                                            let msgs = win.get_messages();
                                            if let Some(vm) = msgs
                                                .as_any()
                                                .downcast_ref::<VecModel<ChatMessage>>()
                                            {
                                                while vm.row_count() > 0 {
                                                    vm.remove(0);
                                                }
                                            }
                                            win.set_streaming_text(SharedString::new());
                                            win.set_streaming_messages(ModelRc::new(VecModel::<
                                                ChatMessage,
                                            >::default(
                                            )));
                                            win.set_thinking_text(SharedString::new());
                                        }
                                    });
                                }
                                ImmediateAction::NewConversation => {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(win) = w.upgrade() {
                                            win.invoke_new_session();
                                        }
                                    });
                                }
                                ImmediateAction::Abort => {
                                    let ch = Arc::clone(&ch3);
                                    tokio::spawn(async move {
                                        if let Some(sender) = ch.lock().await.take() {
                                            let _ = sender.send(());
                                        }
                                    });
                                }
                                ImmediateAction::OpenTeamPicker => {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(win) = w.upgrade() {
                                            win.set_team_picker_visible(true);
                                        }
                                    });
                                }
                                ImmediateAction::OpenInspector { kind } => {
                                    let tab = match kind.title() {
                                        "Skills" => 0,
                                        "Subagents" => 1,
                                        "Peers" => 2,
                                        "Context" => 3,
                                        "Tools" => 4,
                                        "Mcp" | "MCP" => 5,
                                        _ => 0,
                                    };
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(win) = w.upgrade() {
                                            win.set_inspector_tab(tab);
                                            win.set_inspector_visible(true);
                                        }
                                    });
                                }
                                _ => {}
                            }
                        }

                        if let Some(msg) = result.message_to_send {
                            if agent_busy {
                                queue.lock().unwrap().push(QueuedMessage {
                                    content: msg,
                                    model_transition: None,
                                    mode_transition: result.mode_override,
                                });
                                let len = sync_queue_model(&queue, &queue_items_send);
                                let w = weak_send.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(win) = w.upgrade() {
                                        win.set_queue_count(len as i32);
                                    }
                                });
                            } else {
                                for m in markdown_to_plain_messages(&msg, "user") {
                                    pm_send.lock().unwrap().push_back(m);
                                }
                                let sid = active_sid_send.lock().unwrap().clone();
                                let sid_flush = sid.clone();
                                let _ = slint::invoke_from_event_loop({
                                    let pm2 = Arc::clone(&pm_send);
                                    let sm = Arc::clone(&session_msgs_send);
                                    let w = weak_send.clone();
                                    move || flush_messages_to_session(pm2, &sid_flush, sm, &w)
                                });
                                *streaming_sid.lock().unwrap() = Some(sid);
                                let tx = tx.clone();
                                let mode_ov = result.mode_override;
                                let model_ov = result
                                    .model_override
                                    .as_deref()
                                    .map(|s| sven_model::resolve_model_from_config(&config, s));
                                tokio::spawn(async move {
                                    let _ = tx
                                        .send(AgentRequest::Submit {
                                            content: msg,
                                            model_override: model_ov,
                                            mode_override: mode_ov,
                                        })
                                        .await;
                                });
                            }
                        } else if result.model_override.is_some() || result.mode_override.is_some()
                        {
                            if let Some(m) = &result.model_override {
                                let resolved = sven_model::resolve_model_from_config(&config, m);
                                *cur_model.lock().unwrap() = resolved.name.clone();
                                *cur_provider.lock().unwrap() = resolved.provider.clone();
                                let name = resolved.name.clone();
                                let w = weak_send.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(win) = w.upgrade() {
                                        win.set_model_name(SharedString::from(&name));
                                    }
                                });
                            }
                            if let Some(mode) = result.mode_override {
                                *cur_mode.lock().unwrap() = mode;
                                let mode_str = format!("{mode:?}").to_lowercase();
                                let w = weak_send.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(win) = w.upgrade() {
                                        win.set_mode(SharedString::from(&mode_str));
                                    }
                                });
                            }
                        }
                        return;
                    }
                }

                // Enqueue while busy
                if agent_busy {
                    queue.lock().unwrap().push(QueuedMessage {
                        content: content.clone(),
                        model_transition: None,
                        mode_transition: None,
                    });
                    let len = sync_queue_model(&queue, &queue_items_send);
                    if let Some(win) = weak_send.upgrade() {
                        win.set_queue_count(len as i32);
                    }
                    return;
                }

                // Send immediately
                for m in markdown_to_plain_messages(&content, "user") {
                    pm_send.lock().unwrap().push_back(m);
                }
                let sid = active_sid_send.lock().unwrap().clone();
                let _ = slint::invoke_from_event_loop({
                    let pm2 = Arc::clone(&pm_send);
                    let sm = Arc::clone(&session_msgs_send);
                    let w = weak_send.clone();
                    move || flush_messages_to_session(pm2, &sid, sm, &w)
                });

                let gen_title = {
                    let mut first = is_first.lock().unwrap();
                    if *first {
                        *first = false;
                        true
                    } else {
                        false
                    }
                };

                let sid = active_sid_send.lock().unwrap().clone();
                *streaming_sid.lock().unwrap() = Some(sid.clone());
                let tx_clone = tx.clone();
                let tx3 = tx2.clone();
                let mode_val = *cur_mode.lock().unwrap();
                tokio::spawn(async move {
                    let _ = tx_clone
                        .send(AgentRequest::Submit {
                            content: content.clone(),
                            model_override: None,
                            mode_override: Some(mode_val),
                        })
                        .await;
                    if gen_title {
                        let _ = tx3
                            .send(AgentRequest::GenerateTitle { user_text: content })
                            .await;
                    }
                });
            });
        }

        // ── Cancel run ────────────────────────────────────────────────────────
        {
            let ch = Arc::clone(&cancel_handle);
            window.on_cancel_run(move || {
                let ch = Arc::clone(&ch);
                tokio::spawn(async move {
                    if let Some(sender) = ch.lock().await.take() {
                        let _ = sender.send(());
                    }
                });
            });
        }

        // ── Queue panel actions ───────────────────────────────────────────────
        {
            let tx_q = agent_tx.clone();
            let pm_q = Arc::clone(&pending_msgs);
            let sm_q = Arc::clone(&session_messages);
            let active_q = Arc::clone(&active_session_id);
            let streaming_q = Arc::clone(&streaming_session_id);
            let cur_mode_q = Arc::clone(&current_mode);
            let weak_q = weak.clone();

            window.on_queue_edit_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q2 = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move |idx| {
                    let idx = idx as usize;
                    let content = {
                        let mut q = queue_q.lock().unwrap();
                        if idx < q.messages.len() {
                            let c = q.messages[idx].content.clone();
                            q.messages.remove(idx);
                            Some(c)
                        } else {
                            None
                        }
                    };
                    if let Some(content) = content {
                        let len = sync_queue_model(&queue_q, &qi_q);
                        if let Some(win) = weak_q2.upgrade() {
                            win.set_input_text(SharedString::from(&content));
                            win.set_queue_count(len as i32);
                        }
                    }
                }
            });

            window.on_queue_delete_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q2 = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move |idx| {
                    let idx = idx as usize;
                    {
                        let mut q = queue_q.lock().unwrap();
                        if idx < q.messages.len() {
                            q.messages.remove(idx);
                        }
                    }
                    let len = sync_queue_model(&queue_q, &qi_q);
                    if let Some(win) = weak_q2.upgrade() {
                        win.set_queue_count(len as i32);
                    }
                }
            });

            window.on_queue_clear_all({
                let queue_q = Arc::clone(&queue_state);
                let weak_q2 = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move || {
                    queue_q.lock().unwrap().messages.clear();
                    qi_q.clear();
                    if let Some(win) = weak_q2.upgrade() {
                        win.set_queue_count(0);
                    }
                }
            });

            window.on_queue_submit_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q2 = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                let cancel_handle2 = Arc::clone(&cancel_handle);
                move |idx| {
                    let idx = idx as usize;
                    let qm = {
                        let mut q = queue_q.lock().unwrap();
                        q.messages.remove(idx)
                    };
                    let agent_busy = weak_q2
                        .upgrade()
                        .map(|w| w.get_agent_busy())
                        .unwrap_or(false);
                    let len = sync_queue_model(&queue_q, &qi_q);
                    if let Some(win) = weak_q2.upgrade() {
                        win.set_queue_count(len as i32);
                    }

                    if let Some(qm) = qm {
                        if agent_busy {
                            queue_q.lock().unwrap().messages.push_front(qm);
                            let ch = Arc::clone(&cancel_handle2);
                            tokio::spawn(async move {
                                if let Some(sender) = ch.lock().await.take() {
                                    let _ = sender.send(());
                                }
                            });
                        } else {
                            let content = qm.content.clone();
                            let mode_val = *cur_mode_q.lock().unwrap();
                            for m in markdown_to_plain_messages(&content, "user") {
                                pm_q.lock().unwrap().push_back(m);
                            }
                            let sid = active_q.lock().unwrap().clone();
                            *streaming_q.lock().unwrap() = Some(sid.clone());
                            let tx = tx_q.clone();
                            let _ = slint::invoke_from_event_loop({
                                let pm2 = Arc::clone(&pm_q);
                                let sm2 = Arc::clone(&sm_q);
                                let w2 = weak_q.clone();
                                move || flush_messages_to_session(pm2, &sid, sm2, &w2)
                            });
                            tokio::spawn(async move {
                                let _ = tx
                                    .send(AgentRequest::Submit {
                                        content,
                                        model_override: None,
                                        mode_override: Some(mode_val),
                                    })
                                    .await;
                            });
                        }
                    }
                }
            });
        }

        // ── New session ───────────────────────────────────────────────────────
        {
            let msgs_ns = Rc::clone(&msgs_model);
            let sessions_ns = Rc::clone(&sessions_model);
            let is_first_ns = Arc::clone(&is_first_message);
            let queue_ns = Arc::clone(&queue_state);
            let qi_ns = Rc::clone(&queue_items_model);
            let sb_ns = Arc::clone(&streaming_buf);
            let tb_ns = Arc::clone(&thinking_buf);
            let weak_ns = weak.clone();
            let session_msgs_ns = Arc::clone(&session_messages);
            let session_usage_ns = Arc::clone(&session_usage);
            let active_sid_ns = Arc::clone(&active_session_id);

            window.on_new_session(move || {
                let current_id = active_sid_ns.lock().unwrap().clone();
                {
                    let current_msgs: Vec<PlainChatMessage> = (0..msgs_ns.row_count())
                        .filter_map(|i| msgs_ns.row_data(i))
                        .map(|m| slint_msg_to_plain(&m))
                        .collect();
                    if !current_msgs.is_empty() {
                        let title = (0..sessions_ns.row_count())
                            .find_map(|i| sessions_ns.row_data(i).filter(|s| s.id == current_id))
                            .map(|s| s.title.to_string())
                            .unwrap_or_else(|| "Chat".to_string());
                        let usage = session_usage_ns.lock().unwrap().get(&current_id).cloned();
                        save_session_to_disk(&current_id, &current_msgs, &title, None, None, usage);
                    }
                    // Evict outgoing session from memory; reload lazily from disk if needed.
                    session_msgs_ns.lock().unwrap().remove(&current_id);
                }

                while msgs_ns.row_count() > 0 {
                    msgs_ns.remove(0);
                }

                *sb_ns.lock().unwrap() = String::new();
                *tb_ns.lock().unwrap() = String::new();
                *queue_ns.lock().unwrap() = QueueState::new();
                qi_ns.clear();
                *is_first_ns.lock().unwrap() = true;

                let new_id = SessionId::new().as_str().to_string();

                for i in 0..sessions_ns.row_count() {
                    if let Some(mut s) = sessions_ns.row_data(i) {
                        s.active = false;
                        sessions_ns.set_row_data(i, s);
                    }
                }

                sessions_ns.insert(
                    0,
                    SessionItem {
                        id: SharedString::from(new_id.clone()),
                        title: SharedString::from("New chat"),
                        busy: false,
                        active: true,
                        depth: 0,
                        status: SharedString::from("active"),
                        total_cost_usd: 0.0,
                    },
                );

                *active_sid_ns.lock().unwrap() = new_id.clone();

                if let Some(win) = weak_ns.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id));
                    win.set_streaming_text(SharedString::new());
                    win.set_streaming_messages(ModelRc::new(VecModel::<ChatMessage>::default()));
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                    win.set_queue_count(0);
                    // New blank session starts with zero counters.
                    win.set_total_cost_usd(0.0);
                    win.set_total_output_tokens(0);
                    win.set_total_input_tokens(0);
                    win.set_context_pct(0);
                }
            });
        }

        // ── Session delete ────────────────────────────────────────────────────
        {
            let sessions_del = Rc::clone(&sessions_model);
            let session_msgs_del = Arc::clone(&session_messages);
            let active_del = Arc::clone(&active_session_id);
            let weak_del = weak.clone();

            window.on_session_delete_requested(move |id| {
                let id_str = id.to_string();
                let is_active = active_del.lock().unwrap().clone() == id_str;

                // Remove from sessions list
                for i in 0..sessions_del.row_count() {
                    if let Some(s) = sessions_del.row_data(i) {
                        if s.id == id {
                            sessions_del.remove(i);
                            break;
                        }
                    }
                }

                // Remove from memory
                session_msgs_del.lock().unwrap().remove(&id_str);

                // Delete from disk
                delete_session_from_disk(&id_str);

                // If active session was deleted, switch to new session
                if is_active {
                    if let Some(win) = weak_del.upgrade() {
                        win.invoke_new_session();
                    }
                }
            });
        }

        // ── Session rename ────────────────────────────────────────────────────
        {
            let sessions_ren = Rc::clone(&sessions_model);
            window.on_session_rename_requested(move |id, new_title| {
                let id_str = id.to_string();
                for i in 0..sessions_ren.row_count() {
                    if let Some(mut s) = sessions_ren.row_data(i) {
                        if s.id == id_str {
                            s.title = new_title.clone();
                            sessions_ren.set_row_data(i, s);
                            break;
                        }
                    }
                }
            });
        }

        // ── Question modal state ──────────────────────────────────────────────
        struct PendingQuestion {
            session_id: String,
            questions: Vec<sven_tools::Question>,
            answer_tx: tokio::sync::oneshot::Sender<String>,
        }
        let pending_question: Arc<Mutex<Option<PendingQuestion>>> = Arc::new(Mutex::new(None));
        let question_items_model = Rc::new(VecModel::<QuestionItem>::default());
        let question_current_index: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));

        std::thread_local! {
            static QUESTION_ITEMS_MODEL: std::cell::RefCell<Option<Rc<VecModel<QuestionItem>>>> =
                const { std::cell::RefCell::new(None) };
        }
        QUESTION_ITEMS_MODEL.with(|tl| *tl.borrow_mut() = Some(Rc::clone(&question_items_model)));

        fn questions_to_slint(questions: &[sven_tools::Question]) -> Vec<QuestionItem> {
            questions
                .iter()
                .map(|q| QuestionItem {
                    prompt: SharedString::from(&q.prompt),
                    options: slint::ModelRc::new(slint::VecModel::from(
                        q.options
                            .iter()
                            .map(|o| SharedString::from(o.as_str()))
                            .collect::<Vec<_>>(),
                    )),
                    selected_index: -1,
                    other_text: SharedString::new(),
                    allow_multiple: q.allow_multiple,
                })
                .collect()
        }

        fn build_answer_string(
            questions: &[sven_tools::Question],
            items: &[QuestionItem],
        ) -> String {
            questions
                .iter()
                .zip(items.iter())
                .map(|(q, item)| {
                    let ans = if item.selected_index < 0 {
                        "(no selection)".to_string()
                    } else if item.selected_index as usize == q.options.len() {
                        if item.other_text.to_string().trim().is_empty() {
                            "Other".to_string()
                        } else {
                            format!("Other: {}", item.other_text.to_string().trim())
                        }
                    } else if let Some(opt) = q.options.get(item.selected_index as usize) {
                        opt.clone()
                    } else {
                        "(invalid)".to_string()
                    };
                    format!("Q: {}\nA: {}", q.prompt, ans)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }

        // ── Session selected ──────────────────────────────────────────────────
        {
            let msgs_ss = Rc::clone(&msgs_model);
            let sessions_ss = Rc::clone(&sessions_model);
            let weak_ss = weak.clone();
            let pending_show = Arc::clone(&pending_question);
            let qi_model_show = Rc::clone(&question_items_model);
            let qidx_show = Arc::clone(&question_current_index);
            let session_msgs_ss = Arc::clone(&session_messages);
            let session_usage_ss = Arc::clone(&session_usage);
            let active_sid_ss = Arc::clone(&active_session_id);
            let sb_ss = Arc::clone(&streaming_buf);
            let tb_ss = Arc::clone(&thinking_buf);
            let cur_model_ss = Arc::clone(&current_model_name);
            let cur_mode_ss = Arc::clone(&current_mode);

            window.on_session_selected(move |id| {
                let new_id = id.to_string();
                let current_id = active_sid_ss.lock().unwrap().clone();
                if new_id == current_id {
                    return;
                }

                {
                    let snaps: Vec<PlainChatMessage> = (0..msgs_ss.row_count())
                        .filter_map(|i| msgs_ss.row_data(i))
                        .map(|m| slint_msg_to_plain(&m))
                        .collect();
                    if !snaps.is_empty() {
                        let title = (0..sessions_ss.row_count())
                            .find_map(|i| sessions_ss.row_data(i).filter(|s| s.id == current_id))
                            .map(|s| s.title.to_string())
                            .unwrap_or_else(|| "Chat".to_string());
                        let model = cur_model_ss.lock().unwrap().clone();
                        let mode = format!("{:?}", *cur_mode_ss.lock().unwrap()).to_lowercase();
                        let usage = session_usage_ss.lock().unwrap().get(&current_id).cloned();
                        save_session_to_disk(
                            &current_id,
                            &snaps,
                            &title,
                            Some(&model),
                            Some(&mode),
                            usage,
                        );
                    }
                    // Evict the outgoing session from memory after saving to disk;
                    // it will be reloaded lazily from disk if visited again.
                    session_msgs_ss.lock().unwrap().remove(&current_id);
                }

                for i in 0..sessions_ss.row_count() {
                    if let Some(mut s) = sessions_ss.row_data(i) {
                        s.active = s.id == id;
                        sessions_ss.set_row_data(i, s);
                    }
                }

                *active_sid_ss.lock().unwrap() = new_id.clone();

                let saved = session_msgs_ss.lock().unwrap().get(&new_id).cloned();
                let saved = match saved {
                    Some(msgs) => msgs,
                    None => {
                        let sid = SessionId::from_string(new_id.clone());
                        let path = chat_path(&sid);
                        if path.exists() {
                            match load_chat_from(&path) {
                                Ok(doc) => {
                                    let msgs = chat_document_to_plain_messages(&doc);
                                    session_msgs_ss
                                        .lock()
                                        .unwrap()
                                        .insert(new_id.clone(), msgs.clone());
                                    msgs
                                }
                                Err(e) => {
                                    tracing::warn!("failed to load chat {}: {e}", path.display());
                                    Vec::new()
                                }
                            }
                        } else {
                            Vec::new()
                        }
                    }
                };

                while msgs_ss.row_count() > 0 {
                    msgs_ss.remove(0);
                }
                for m in &saved {
                    msgs_ss.push(m.to_slint());
                }

                *sb_ss.lock().unwrap() = String::new();
                *tb_ss.lock().unwrap() = String::new();

                // Restore per-session token usage for the new active session.
                let (target_cost, target_out, target_in) = {
                    let map = session_usage_ss.lock().unwrap();
                    let u = map.get(&new_id);
                    (
                        u.map(|u| u.total_cost_usd as f32).unwrap_or(0.0),
                        u.map(|u| u.total_output_tokens as i32).unwrap_or(0),
                        u.map(|u| u.total_input_tokens as i32).unwrap_or(0),
                    )
                };

                if let Some(win) = weak_ss.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id.clone()));
                    win.set_streaming_text(SharedString::new());
                    win.set_streaming_messages(ModelRc::new(VecModel::<ChatMessage>::default()));
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                    win.set_total_cost_usd(target_cost);
                    win.set_total_output_tokens(target_out);
                    win.set_total_input_tokens(target_in);
                    win.set_context_pct(0);
                }

                if let Some(ref pq) = *pending_show.lock().unwrap() {
                    if pq.session_id == new_id {
                        qi_model_show.clear();
                        for item in &questions_to_slint(&pq.questions) {
                            qi_model_show.push(item.clone());
                        }
                        *qidx_show.lock().unwrap() = 0;
                        if let Some(win) = weak_ss.upgrade() {
                            win.set_question_items(ModelRc::from(Rc::clone(&qi_model_show)));
                            win.set_question_current_index(0);
                            win.set_question_visible(true);
                        }
                    }
                }
            });
        }

        // ── Question modal recv ───────────────────────────────────────────────
        {
            let weak_q = weak.clone();
            let pending = Arc::clone(&pending_question);
            let streaming_sid_q = Arc::clone(&streaming_session_id);
            let qidx = Arc::clone(&question_current_index);
            tokio::spawn(async move {
                while let Some(req) = question_rx.recv().await {
                    let session_id = streaming_sid_q.lock().unwrap().clone();
                    let questions = req.questions;
                    let answer_tx = req.answer_tx;
                    let w = weak_q.clone();
                    let p = Arc::clone(&pending);
                    let idx = Arc::clone(&qidx);
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(win) = w.upgrade() {
                            let sid = win.get_active_session_id().to_string();
                            let show = session_id.as_ref() == Some(&sid);
                            *p.lock().unwrap() = Some(PendingQuestion {
                                session_id: session_id.clone().unwrap_or_default(),
                                questions: questions.clone(),
                                answer_tx,
                            });
                            let items = questions_to_slint(&questions);
                            let all_answered = questions.is_empty();
                            QUESTION_ITEMS_MODEL.with(|tl| {
                                if let Some(ref qi) = *tl.borrow() {
                                    qi.clear();
                                    for item in &items {
                                        qi.push(item.clone());
                                    }
                                }
                            });
                            *idx.lock().unwrap() = 0;
                            QUESTION_ITEMS_MODEL.with(|tl| {
                                if let Some(ref qi) = *tl.borrow() {
                                    win.set_question_items(ModelRc::from(Rc::clone(qi)));
                                }
                            });
                            win.set_question_current_index(0);
                            win.set_question_all_answered(all_answered);
                            win.set_question_visible(show);
                        }
                    });
                }
            });
        }

        // ── Question callbacks ────────────────────────────────────────────────
        {
            let pending = Arc::clone(&pending_question);
            let qi_model = Rc::clone(&question_items_model);
            let qidx = Arc::clone(&question_current_index);

            window.on_question_option_selected({
                let qi = Rc::clone(&qi_model);
                let weak_qa = weak.clone();
                let pending_qa = Arc::clone(&pending);
                move |qidx_val, opt_idx| {
                    let idx = qidx_val as usize;
                    if let Some(mut row) = qi.row_data(idx) {
                        row.selected_index = opt_idx;
                        qi.set_row_data(idx, row);
                    }
                    // Update all-answered property
                    let all = (0..qi.row_count())
                        .filter_map(|i| qi.row_data(i))
                        .all(|r| r.selected_index >= 0);
                    let pq_len = pending_qa
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|p| p.questions.len())
                        .unwrap_or(0);
                    let all_done = all && pq_len > 0;
                    if let Some(win) = weak_qa.upgrade() {
                        win.set_question_all_answered(all_done);
                    }
                }
            });

            window.on_question_other_changed({
                let qi = Rc::clone(&qi_model);
                move |qidx_val, text| {
                    let idx = qidx_val as usize;
                    if let Some(mut row) = qi.row_data(idx) {
                        row.other_text = text;
                        qi.set_row_data(idx, row);
                    }
                }
            });

            window.on_question_next({
                let qi = Rc::clone(&qi_model);
                let w = weak.clone();
                let qidx = Arc::clone(&qidx);
                move || {
                    let mut idx = qidx.lock().unwrap();
                    *idx = (*idx + 1).min(qi.row_count() as i32 - 1).max(0);
                    if let Some(win) = w.upgrade() {
                        win.set_question_current_index(*idx);
                    }
                }
            });

            window.on_question_back({
                let w = weak.clone();
                let qidx = Arc::clone(&qidx);
                move || {
                    let mut idx = qidx.lock().unwrap();
                    *idx = (*idx - 1).max(0);
                    if let Some(win) = w.upgrade() {
                        win.set_question_current_index(*idx);
                    }
                }
            });

            window.on_question_answered({
                let qi = Rc::clone(&qi_model);
                let w = weak.clone();
                move || {
                    if let Some(pq) = pending.lock().unwrap().take() {
                        let items: Vec<QuestionItem> =
                            (0..qi.row_count()).filter_map(|i| qi.row_data(i)).collect();
                        let formatted = build_answer_string(&pq.questions, &items);
                        let _ = pq.answer_tx.send(formatted);
                    }
                    if let Some(win) = w.upgrade() {
                        win.set_question_visible(false);
                    }
                }
            });

            let pending_d = Arc::clone(&pending_question);
            let weak_d = weak.clone();
            window.on_question_dismissed(move || {
                if let Some(pq) = pending_d.lock().unwrap().take() {
                    let _ = pq.answer_tx.send(
                        "User declined to answer the questions. Proceed with your best judgement."
                            .into(),
                    );
                }
                if let Some(win) = weak_d.upgrade() {
                    win.set_question_visible(false);
                }
            });
        }

        // ── Slash completion ──────────────────────────────────────────────────
        {
            let weak_ic = weak.clone();
            let config_ic = Arc::clone(&opts.config);
            let cur_provider_ic = Arc::clone(&current_model_provider);
            let cur_model_ic = Arc::clone(&current_model_name);

            window.on_input_changed(move |text| {
                let text = text.to_string();
                let registry = CommandRegistry::with_builtins();
                let ctx = CommandContext {
                    config: Arc::clone(&config_ic),
                    current_model_provider: cur_provider_ic.lock().unwrap().clone(),
                    current_model_name: cur_model_ic.lock().unwrap().clone(),
                };

                let parsed = sven_frontend::commands::parser::parse(&text);
                let completions = if matches!(parsed, ParsedCommand::NotCommand) {
                    vec![]
                } else {
                    let mgr = sven_frontend::commands::completion::CompletionManager::new(
                        std::sync::Arc::new(registry),
                    );
                    mgr.get_completions(&parsed, &ctx)
                };

                let entries: Vec<CompletionEntry> = completions
                    .into_iter()
                    .take(12)
                    .map(|c| CompletionEntry {
                        value: SharedString::from(c.value.as_str()),
                        display: SharedString::from(c.display.as_str()),
                        description: SharedString::from(c.description.as_deref().unwrap_or("")),
                    })
                    .collect();

                if let Some(win) = weak_ic.upgrade() {
                    win.set_completion_items(ModelRc::from(Rc::new(VecModel::from(entries))));
                    win.set_completion_selected(0);
                }
            });
        }

        // ── Completion accepted ───────────────────────────────────────────────
        {
            let weak_ca = weak.clone();
            window.on_completion_accepted(move |val| {
                let Some(win) = weak_ca.upgrade() else { return };
                let val_str = val.to_string();

                let apply_val = if val_str.is_empty() {
                    let items = win.get_completion_items();
                    let idx = win.get_completion_selected() as usize;
                    items.row_data(idx).map(|e| e.value.to_string())
                } else {
                    Some(val_str)
                };

                if let Some(v) = apply_val {
                    let current = win.get_input_text().to_string();
                    let new_text = if let Some(stripped) = current.strip_prefix('/') {
                        if v.starts_with('/') {
                            format!("{v} ")
                        } else {
                            let parts: Vec<&str> = stripped.splitn(2, ' ').collect();
                            if parts.len() <= 1 {
                                format!("/{v} ")
                            } else {
                                format!("/{} {v} ", parts[0])
                            }
                        }
                    } else {
                        format!("{v} ")
                    };
                    win.set_input_text(SharedString::from(&new_text));
                    win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
                    win.set_completion_selected(0);
                    if new_text.starts_with('/') && new_text.ends_with(' ') {
                        win.invoke_send_message(SharedString::from(new_text.trim()));
                        win.set_input_text(SharedString::new());
                    }
                } else {
                    win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
                }
            });
        }

        // ── Picker ────────────────────────────────────────────────────────────
        let picker_all_items: Arc<Mutex<Vec<PickerItem>>> = Arc::new(Mutex::new(Vec::new()));

        {
            let weak_mc = weak.clone();
            let config_mc = Arc::clone(&opts.config);
            let cur_provider_mc = Arc::clone(&current_model_provider);
            let cur_model_mc = Arc::clone(&current_model_name);
            let all_mc = Arc::clone(&picker_all_items);

            window.on_model_clicked(move || {
                let current = format!(
                    "{}/{}",
                    cur_provider_mc.lock().unwrap(),
                    cur_model_mc.lock().unwrap()
                );

                let mut items: Vec<PickerItem> = vec![PickerItem {
                    id: SharedString::from(current.clone()),
                    label: SharedString::from(format!("{} ✓", current)),
                    description: SharedString::from("current model"),
                }];

                let mut catalog_models = catalog::static_catalog();
                catalog_models.sort_by(|a, b| a.provider.cmp(&b.provider).then(a.id.cmp(&b.id)));
                for entry in &catalog_models {
                    let id = format!("{}/{}", entry.provider, entry.id);
                    if id == current {
                        continue;
                    }
                    items.push(PickerItem {
                        id: SharedString::from(id),
                        label: SharedString::from(format!("{}/{}", entry.provider, entry.id)),
                        description: SharedString::from(entry.description.as_str()),
                    });
                }

                let mut provider_names: Vec<&str> =
                    config_mc.providers.keys().map(|s| s.as_str()).collect();
                provider_names.sort_unstable();
                for name in provider_names {
                    let cfg = &config_mc.providers[name];
                    for model_name in cfg.models.keys() {
                        let id = format!("{}/{}", name, model_name);
                        if id == current {
                            continue;
                        }
                        items.push(PickerItem {
                            id: SharedString::from(id),
                            label: SharedString::from(format!("{}/{}", name, model_name)),
                            description: SharedString::from(format!("driver: {}", cfg.name)),
                        });
                    }
                }

                *all_mc.lock().unwrap() = items.clone();

                if let Some(win) = weak_mc.upgrade() {
                    win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(items))));
                    win.set_picker_title(SharedString::from("Switch model"));
                    win.set_picker_visible(true);
                    win.set_picker_keyboard_index(0);
                }
            });
        }

        {
            let weak_mc = weak.clone();
            let all_mc = Arc::clone(&picker_all_items);
            let cur_mode_mc = Arc::clone(&current_mode);
            window.on_mode_clicked(move || {
                let items = vec![
                    PickerItem {
                        id: SharedString::from("agent"),
                        label: SharedString::from("agent"),
                        description: SharedString::from("Full agent with read/write tools"),
                    },
                    PickerItem {
                        id: SharedString::from("plan"),
                        label: SharedString::from("plan"),
                        description: SharedString::from("Plan without making code changes"),
                    },
                    PickerItem {
                        id: SharedString::from("research"),
                        label: SharedString::from("research"),
                        description: SharedString::from("Read-only — explores and answers"),
                    },
                ];
                *all_mc.lock().unwrap() = items.clone();
                let mode_str = format!("{:?}", *cur_mode_mc.lock().unwrap()).to_lowercase();
                let idx = items.iter().position(|i| i.id == mode_str).unwrap_or(0);
                if let Some(win) = weak_mc.upgrade() {
                    win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(items))));
                    win.set_picker_title(SharedString::from("Switch mode"));
                    win.set_picker_visible(true);
                    win.set_picker_keyboard_index(idx as i32);
                }
            });
        }

        {
            let weak_ps = weak.clone();
            let cur_mode_ps = Arc::clone(&current_mode);
            let cur_model_ps = Arc::clone(&current_model_name);
            let cur_provider_ps = Arc::clone(&current_model_provider);

            window.on_picker_selected(move |id| {
                let id = id.to_string();
                let Some(win) = weak_ps.upgrade() else { return };
                win.set_picker_visible(false);
                let title = win.get_picker_title().to_string();

                if title == "Switch mode" {
                    let mode = match id.as_str() {
                        "plan" => AgentMode::Plan,
                        "research" => AgentMode::Research,
                        _ => AgentMode::Agent,
                    };
                    *cur_mode_ps.lock().unwrap() = mode;
                    win.set_mode(SharedString::from(id));
                } else {
                    let parts: Vec<&str> = id.splitn(2, '/').collect();
                    if parts.len() == 2 {
                        *cur_provider_ps.lock().unwrap() = parts[0].to_string();
                        *cur_model_ps.lock().unwrap() = parts[1].to_string();
                        win.set_model_name(SharedString::from(parts[1]));
                    } else {
                        *cur_model_ps.lock().unwrap() = id.clone();
                        win.set_model_name(SharedString::from(id));
                    }
                }
            });
        }

        {
            let weak_pd = weak.clone();
            window.on_picker_dismissed(move || {
                if let Some(win) = weak_pd.upgrade() {
                    win.set_picker_visible(false);
                }
            });
        }

        {
            let weak_psc = weak.clone();
            let all = Arc::clone(&picker_all_items);
            window.on_picker_search_changed(move |query| {
                let Some(win) = weak_psc.upgrade() else {
                    return;
                };
                let query_lower = query.to_string().to_lowercase();
                let all_items = all.lock().unwrap();
                let filtered: Vec<PickerItem> = if query_lower.is_empty() {
                    all_items.clone()
                } else {
                    all_items
                        .iter()
                        .filter(|item| {
                            item.label.to_lowercase().as_str().contains(&query_lower)
                                || item
                                    .description
                                    .to_lowercase()
                                    .as_str()
                                    .contains(&query_lower)
                        })
                        .cloned()
                        .collect()
                };
                win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(filtered))));
                win.set_picker_keyboard_index(0);
            });
        }

        // ── Sidebar search ────────────────────────────────────────────────────
        {
            let search_q = Arc::clone(&sidebar_search_query);
            let filter = Rc::clone(&filter_model);
            window.on_sidebar_search_changed(move |query| {
                *search_q.lock().unwrap() = query.to_string();
                filter.reset();
            });
        }

        // ── Inspector ─────────────────────────────────────────────────────────
        {
            let weak_insp = weak.clone();
            window.on_inspector_tab_changed(move |tab| {
                // Populate with placeholder; real data fetching would happen here
                let kind = InspectorKind::from_index(tab);
                let items =
                    items_from_list(&[(kind.title().to_string(), String::new(), String::new())]);
                if let Some(win) = weak_insp.upgrade() {
                    win.set_inspector_items(items);
                }
            });
        }

        // ── Team agent ────────────────────────────────────────────────────────
        {
            window.on_team_agent_selected(move |_id| {
                // Team agent switching can be expanded in the future
            });
        }

        // ── Event bridge ──────────────────────────────────────────────────────
        let pm = Arc::clone(&pending_msgs);
        let pt = Arc::clone(&pending_toasts);
        let sb = Arc::clone(&streaming_buf);
        let tb = Arc::clone(&thinking_buf);
        let session_usage_ev = Arc::clone(&session_usage);
        let queue_ev = Arc::clone(&queue_state);
        let tx_ev = agent_tx.clone();
        let weak2 = weak.clone();
        let session_msgs_ev = Arc::clone(&session_messages);
        let streaming_sid_ev = Arc::clone(&streaming_session_id);
        let active_sid_ev = Arc::clone(&active_session_id);
        let cur_model_ev = Arc::clone(&current_model_name);
        let cur_mode_ev = Arc::clone(&current_mode);

        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgentEvent::TextDelta(delta) => {
                        let mut buf = sb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = strip_inline_markdown(&buf);
                        let parsed = markdown_to_plain_messages(&text, "assistant");
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let text_clone = text.clone();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            let active = active_sid_ev.lock().unwrap().clone();
                            let plain = parsed;
                            move || {
                                if let Some(win) = w.upgrade() {
                                    if sid.as_ref() == Some(&active) {
                                        if !win.get_thinking_text().is_empty() {
                                            win.set_thinking_text(SharedString::new());
                                        }
                                        win.set_streaming_text(SharedString::from(text_clone));
                                        let slint_msgs: Vec<ChatMessage> =
                                            plain.iter().map(|p| p.to_slint()).collect();
                                        win.set_streaming_messages(ModelRc::new(VecModel::from(
                                            slint_msgs,
                                        )));
                                    }
                                    win.set_agent_busy(true);
                                }
                            }
                        });
                    }

                    AgentEvent::TextComplete(text) => {
                        *sb.lock().unwrap() = String::new();
                        let msgs = markdown_to_plain_messages(&text, "assistant");
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        {
                            let mut q = pm.lock().unwrap();
                            for m in msgs {
                                q.push_back(m);
                            }
                        }
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_streaming_messages(ModelRc::new(
                                        VecModel::<ChatMessage>::default(),
                                    ));
                                    win.set_thinking_text(SharedString::new());
                                }
                            }
                        });
                    }

                    AgentEvent::ThinkingDelta(delta) => {
                        let mut buf = tb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = strip_inline_markdown(&buf);
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            let active = active_sid_ev.lock().unwrap().clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    if sid.as_ref() == Some(&active) {
                                        win.set_thinking_text(SharedString::from(text));
                                    }
                                    win.set_agent_busy(true);
                                }
                            }
                        });
                    }

                    AgentEvent::ThinkingComplete(content) => {
                        *tb.lock().unwrap() = String::new();
                        let stripped = strip_inline_markdown(&content);
                        let preview = stripped
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .to_string();
                        let sub_blocks = markdown_to_md_blocks(&content);
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "thinking",
                            content: stripped,
                            thinking_preview: preview,
                            role: "thinking",
                            is_first_in_group: false,
                            is_expanded: false,
                            sub_blocks,
                            ..Default::default()
                        });
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                                if let Some(win) = w.upgrade() {
                                    win.set_thinking_text(SharedString::new());
                                }
                            }
                        });
                    }

                    AgentEvent::ToolCallStarted(tc_call) => {
                        use sven_frontend::tool_view::extract_tool_view;
                        let view = extract_tool_view(&tc_call.name, &tc_call.args, None);
                        let (fields_json, is_expanded) = if tc_call.name == "todo" {
                            let todos_from_args: Vec<TodoItem> = tc_call
                                .args
                                .get("todos")
                                .and_then(|v| v.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|v| serde_json::from_value(v.clone()).ok())
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            let sid = streaming_sid_ev.lock().unwrap().clone();
                            if let Some(ref s) = sid {
                                current_todos
                                    .lock()
                                    .unwrap()
                                    .insert(s.clone(), todos_from_args.clone());
                            }
                            let todos = if todos_from_args.is_empty() {
                                sid.as_ref()
                                    .and_then(|s| current_todos.lock().unwrap().get(s).cloned())
                                    .unwrap_or_default()
                            } else {
                                todos_from_args
                            };
                            (format_todos_display(&todos), true)
                        } else {
                            (format_fields_json(&view.fields), false)
                        };
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "tool-call",
                            content: tc_call.args.to_string(),
                            role: "assistant",
                            tool_name: tc_call.name.clone(),
                            tool_icon: view.icon,
                            tool_summary: view.summary,
                            tool_category: view.category,
                            tool_fields_json: fields_json,
                            is_expanded,
                            ..Default::default()
                        });
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
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
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                // Attach result to the last tool-call message in both
                                // the session store and the Slint window model.
                                if let Some(ref s) = sid_clone {
                                    attach_tool_result_to_last_call(&preview, is_error, s, &sm, &w);
                                } else if let Some(win) = w.upgrade() {
                                    attach_tool_result_in_window(&win, &preview, is_error);
                                }
                            }
                        });
                    }

                    AgentEvent::TodoUpdate(todos) => {
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        if let Some(ref s) = sid {
                            current_todos
                                .lock()
                                .unwrap()
                                .insert(s.clone(), todos.clone());
                        }
                        let sid_clone = sid.clone();
                        let todos_clone = todos.clone();
                        let w2 = weak2.clone();
                        let active = active_sid_ev.lock().unwrap().clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(win) = w2.upgrade() {
                                if sid_clone.as_ref() == Some(&active) {
                                    update_last_todo_tool_call(
                                        &win,
                                        &format_todos_display(&todos_clone),
                                    );
                                }
                            }
                        });
                    }

                    AgentEvent::TurnComplete => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();

                        let next = queue_ev.lock().unwrap().pop_front();
                        let queue_len = queue_ev.lock().unwrap().len();

                        if let Some(queued) = next {
                            let sid = active_sid_ev.lock().unwrap().clone();
                            *streaming_sid_ev.lock().unwrap() = Some(sid);
                            let tx = tx_ev.clone();
                            tokio::spawn(async move {
                                let _ = tx
                                    .send(AgentRequest::Submit {
                                        content: queued.content,
                                        model_override: queued
                                            .model_transition
                                            .map(|d| d.into_model_config()),
                                        mode_override: queued.mode_transition,
                                    })
                                    .await;
                            });
                        } else {
                            *streaming_sid_ev.lock().unwrap() = None;
                        }

                        let model = cur_model_ev.lock().unwrap().clone();
                        let mode = format!("{:?}", *cur_mode_ev.lock().unwrap()).to_lowercase();
                        let sm = Arc::clone(&session_msgs_ev);
                        let active_usage = {
                            let active = active_sid_ev.lock().unwrap().clone();
                            session_usage_ev.lock().unwrap().get(&active).cloned()
                        };
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            let queue_empty = queue_len == 0;
                            let active = active_sid_ev.lock().unwrap().clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_streaming_messages(ModelRc::new(
                                        VecModel::<ChatMessage>::default(),
                                    ));
                                    win.set_thinking_text(SharedString::new());
                                    if queue_empty {
                                        win.set_agent_busy(false);
                                    }
                                    win.set_queue_count(queue_len as i32);
                                    if let Some(plain) = sm.lock().unwrap().get(&active) {
                                        if !plain.is_empty() {
                                            let sessions = win.get_sessions();
                                            let title = (0..sessions.row_count())
                                                .find_map(|i| {
                                                    sessions.row_data(i).filter(|s| s.id == active)
                                                })
                                                .map(|s| s.title.to_string())
                                                .unwrap_or_else(|| "Chat".to_string());
                                            save_session_to_disk(
                                                &active,
                                                plain,
                                                &title,
                                                Some(&model),
                                                Some(&mode),
                                                active_usage.clone(),
                                            );
                                        }
                                    }
                                }
                            }
                        });
                    }

                    AgentEvent::Aborted { .. } => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();
                        *streaming_sid_ev.lock().unwrap() = None;
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_streaming_messages(ModelRc::new(
                                        VecModel::<ChatMessage>::default(),
                                    ));
                                    win.set_thinking_text(SharedString::new());
                                    win.set_agent_busy(false);
                                }
                            }
                        });
                    }

                    AgentEvent::Error(err_msg) => {
                        *sb.lock().unwrap() = String::new();
                        *tb.lock().unwrap() = String::new();
                        pm.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::error(&err_msg));
                        pt.lock().unwrap().push_back(PlainToast {
                            message: err_msg,
                            level: "error",
                        });
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let pt2 = Arc::clone(&pt);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                                flush_toasts(pt2, &w);
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_streaming_messages(ModelRc::new(
                                        VecModel::<ChatMessage>::default(),
                                    ));
                                    win.set_thinking_text(SharedString::new());
                                    win.set_agent_busy(false);
                                }
                            }
                        });
                        *streaming_sid_ev.lock().unwrap() = None;
                    }

                    AgentEvent::TokenUsage {
                        input,
                        output,
                        cache_read,
                        cache_write,
                        cache_read_total: _,
                        cache_write_total: _,
                        max_tokens,
                        max_output_tokens,
                        cost_usd,
                    } => {
                        let ctx_pct = if max_tokens > 0 {
                            let budget = max_tokens.saturating_sub(max_output_tokens);
                            let prompt = input + cache_read + cache_write;
                            ((prompt as f64 / budget as f64) * 100.0).clamp(0.0, 100.0) as i32
                        } else {
                            0
                        };
                        // Accumulate into per-session usage map.
                        let sid = streaming_sid_ev
                            .lock()
                            .unwrap()
                            .clone()
                            .unwrap_or_else(|| active_sid_ev.lock().unwrap().clone());
                        let (cost_f32, out_tokens, in_tokens) = {
                            let mut map = session_usage_ev.lock().unwrap();
                            let u = map.entry(sid.clone()).or_default();
                            if output > 0 {
                                u.total_output_tokens =
                                    u.total_output_tokens.saturating_add(output as u64);
                            }
                            if input > 0 {
                                u.total_input_tokens =
                                    u.total_input_tokens.saturating_add(input as u64);
                            }
                            u.total_cache_read_tokens =
                                u.total_cache_read_tokens.saturating_add(cache_read as u64);
                            u.total_cache_write_tokens = u
                                .total_cache_write_tokens
                                .saturating_add(cache_write as u64);
                            if let Some(c) = cost_usd {
                                u.total_cost_usd += c;
                            }
                            (
                                u.total_cost_usd as f32,
                                u.total_output_tokens as i32,
                                u.total_input_tokens as i32,
                            )
                        };
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_context_pct(ctx_pct);
                                    win.set_total_cost_usd(cost_f32);
                                    win.set_total_output_tokens(out_tokens);
                                    win.set_total_input_tokens(in_tokens);
                                }
                            }
                        });
                    }

                    AgentEvent::TitleGenerated(title) => {
                        let _ = slint::invoke_from_event_loop({
                            let title = sven_input::sanitize_llm_title(&title);
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    let sessions = win.get_sessions();
                                    for i in 0..sessions.row_count() {
                                        if let Some(mut row) = sessions.row_data(i) {
                                            if row.active {
                                                row.title = SharedString::from(&title);
                                                sessions.set_row_data(i, row);
                                                break;
                                            }
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
                        pm.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::system(format!(
                            "Context compacted ({strategy}): {tokens_before}→{tokens_after} tokens"
                        )));
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                            }
                        });
                    }

                    AgentEvent::CollabEvent(ev) => {
                        let text = sven_core::prompts::format_collab_event(&ev);
                        pm.lock().unwrap().push_back(PlainChatMessage::system(text));
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                            }
                        });
                    }

                    AgentEvent::DelegateSummary {
                        to_name,
                        task_title,
                        status,
                        result_preview,
                        ..
                    } => {
                        pm.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::system(format!(
                            "Delegated \"{task_title}\" to {to_name}: {status} — {result_preview}"
                        )));
                        let sid = streaming_sid_ev.lock().unwrap().clone();
                        let sid_clone = sid.clone();
                        let sm = Arc::clone(&session_msgs_ev);
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || {
                                if let Some(ref s) = sid_clone {
                                    flush_messages_to_session(pm2, s, sm, &w);
                                } else {
                                    flush_messages(pm2, &w);
                                }
                            }
                        });
                    }

                    _ => {}
                }

                // Update session busy indicator
                let _ = slint::invoke_from_event_loop({
                    let w = weak2.clone();
                    move || {
                        if let Some(win) = w.upgrade() {
                            let busy = win.get_agent_busy();
                            let sessions = win.get_sessions();
                            for i in 0..sessions.row_count() {
                                if let Some(mut s) = sessions.row_data(i) {
                                    if s.active {
                                        s.busy = busy;
                                        sessions.set_row_data(i, s);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                });
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

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert plain messages to sven_model messages for Resubmit.
fn plain_messages_to_sven_messages(plain: &[PlainChatMessage]) -> Vec<SvenMessage> {
    use crate::sessions::block_to_markdown;
    use sven_frontend::segment::ChatSegment;

    let mut segments: Vec<ChatSegment> = Vec::new();
    let mut assistant_buf = String::new();
    let mut user_blocks: Vec<PlainChatMessage> = Vec::new();
    let mut last_tool_call_id: Option<String> = None;
    let mut tool_call_counter = 0u32;

    let flush_user = |segments: &mut Vec<ChatSegment>, blocks: &mut Vec<PlainChatMessage>| {
        if !blocks.is_empty() {
            let md = crate::sessions::user_blocks_to_markdown(blocks);
            segments.push(ChatSegment::Message(SvenMessage::user(&md)));
            blocks.clear();
        }
    };

    for p in plain {
        let is_user_block = p.role == "user";

        match p.message_type {
            "user" if is_user_block => {
                if !assistant_buf.is_empty() {
                    segments.push(ChatSegment::Message(SvenMessage::assistant(
                        std::mem::take(&mut assistant_buf),
                    )));
                }
                flush_user(&mut segments, &mut user_blocks);
                user_blocks.push(p.clone());
            }
            "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row"
                if is_user_block =>
            {
                user_blocks.push(p.clone());
            }
            "assistant" | "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row" => {
                flush_user(&mut segments, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    assistant_buf.push_str("\n\n");
                }
                assistant_buf.push_str(&block_to_markdown(p));
            }
            "tool-call" => {
                flush_user(&mut segments, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    segments.push(ChatSegment::Message(SvenMessage::assistant(
                        std::mem::take(&mut assistant_buf),
                    )));
                }
                let id = format!("call_{}", tool_call_counter);
                tool_call_counter += 1;
                last_tool_call_id = Some(id.clone());
                segments.push(ChatSegment::Message(SvenMessage {
                    role: Role::Assistant,
                    content: MessageContent::ToolCall {
                        tool_call_id: id,
                        function: FunctionCall {
                            name: p.tool_name.clone(),
                            arguments: p.content.clone(),
                        },
                    },
                }));
            }
            "tool-result" => {
                flush_user(&mut segments, &mut user_blocks);
                if let Some(id) = last_tool_call_id.take() {
                    segments.push(ChatSegment::Message(SvenMessage::tool_result(
                        id, &p.content,
                    )));
                }
            }
            "system" => {
                flush_user(&mut segments, &mut user_blocks);
                if !assistant_buf.is_empty() {
                    segments.push(ChatSegment::Message(SvenMessage::assistant(
                        std::mem::take(&mut assistant_buf),
                    )));
                }
                segments.push(ChatSegment::Message(SvenMessage::system(&p.content)));
            }
            _ => {
                flush_user(&mut segments, &mut user_blocks);
            }
        }
    }
    flush_user(&mut segments, &mut user_blocks);
    if !assistant_buf.is_empty() {
        segments.push(ChatSegment::Message(SvenMessage::assistant(assistant_buf)));
    }
    sven_frontend::messages_for_resubmit(&segments)
}

/// Clear is-search-match flag on all messages.
fn clear_search_highlights(model: &Rc<VecModel<ChatMessage>>) {
    for i in 0..model.row_count() {
        if let Some(mut row) = model.row_data(i) {
            if row.is_search_match {
                row.is_search_match = false;
                model.set_row_data(i, row);
            }
        }
    }
}

/// Parse a tool result string into markdown blocks, falling back to a single
/// plain paragraph when the content doesn't parse as structured markdown.
fn build_tool_result_blocks(result: &str) -> Vec<PlainMdBlock> {
    let blocks = markdown_to_md_blocks(result);
    if blocks.is_empty() {
        vec![PlainMdBlock {
            kind: "paragraph",
            content: result.to_string(),
            ..Default::default()
        }]
    } else {
        blocks
    }
}

/// Attach a tool result to the last tool-call message in the Slint window model.
fn attach_tool_result_in_window(win: &MainWindow, result: &str, is_error: bool) {
    let result_blocks = build_tool_result_blocks(result);
    let msgs_rc = win.get_messages();
    if let Some(vec_model) = msgs_rc.as_any().downcast_ref::<VecModel<ChatMessage>>() {
        let n = vec_model.row_count();
        for i in (0..n).rev() {
            if let Some(mut row) = vec_model.row_data(i) {
                if row.message_type == "tool-call" {
                    row.tool_result_content = SharedString::from(result);
                    row.tool_result_is_error = is_error;
                    row.tool_result_blocks = ModelRc::new(VecModel::from(
                        result_blocks
                            .iter()
                            .map(|b| b.to_slint())
                            .collect::<Vec<MdBlock>>(),
                    ));
                    vec_model.set_row_data(i, row);
                    break;
                }
            }
        }
    }
}

/// Attach a tool result to the last tool-call message in both the session store
/// and (if the session is active) the Slint window model.
fn attach_tool_result_to_last_call(
    result: &str,
    is_error: bool,
    session_id: &str,
    session_messages: &Arc<Mutex<HashMap<String, Vec<PlainChatMessage>>>>,
    weak: &slint::Weak<MainWindow>,
) {
    let result_blocks = build_tool_result_blocks(result);
    // Update session store
    {
        let mut store = session_messages.lock().unwrap();
        if let Some(msgs) = store.get_mut(session_id) {
            for msg in msgs.iter_mut().rev() {
                if msg.message_type == "tool-call" {
                    msg.tool_result_content = result.to_string();
                    msg.tool_result_is_error = is_error;
                    msg.tool_result_blocks = result_blocks.clone();
                    break;
                }
            }
        }
    }
    // Update Slint model if this session is active
    let Some(win) = weak.upgrade() else { return };
    if win.get_active_session_id() == session_id {
        attach_tool_result_in_window(&win, result, is_error);
    }
}

/// Update the last todo tool-call message with formatted todo list.
fn update_last_todo_tool_call(win: &MainWindow, formatted: &str) {
    let msgs_rc = win.get_messages();
    if let Some(vec_model) = msgs_rc.as_any().downcast_ref::<VecModel<ChatMessage>>() {
        let n = vec_model.row_count();
        for i in (0..n).rev() {
            if let Some(mut row) = vec_model.row_data(i) {
                if row.message_type == "tool-call" && row.tool_name == "todo" {
                    row.tool_fields_json = SharedString::from(formatted);
                    vec_model.set_row_data(i, row);
                    break;
                }
            }
        }
    }
}

/// Move a session to the top of the sidebar list.
/// Uses the underlying VecModel from thread-local (must run on main/Slint thread).
fn move_session_to_top(session_id: &str) {
    SESSIONS_MODEL.with(|tl| {
        if let Some(ref sessions_model) = *tl.borrow() {
            for i in 0..sessions_model.row_count() {
                if let Some(row) = sessions_model.row_data(i) {
                    if row.id == session_id && i > 0 {
                        let item = sessions_model.remove(i);
                        sessions_model.insert(0, item);
                        break;
                    }
                }
            }
        }
    });
}

/// Drain pending messages into session store and optionally the window.
fn flush_messages_to_session(
    pending: Arc<Mutex<VecDeque<PlainChatMessage>>>,
    session_id: &str,
    session_messages: Arc<Mutex<HashMap<String, Vec<PlainChatMessage>>>>,
    weak: &slint::Weak<MainWindow>,
) {
    let Some(win) = weak.upgrade() else { return };
    let mut queue = pending.lock().unwrap();
    if queue.is_empty() {
        return;
    }
    let active_id = win.get_active_session_id().to_string();
    let to_push: Vec<PlainChatMessage> = queue.drain(..).collect();
    session_messages
        .lock()
        .unwrap()
        .entry(session_id.to_string())
        .or_default()
        .extend(to_push.iter().cloned());
    if session_id == active_id {
        let msgs_rc = win.get_messages();
        if let Some(vec_model) = msgs_rc.as_any().downcast_ref::<VecModel<ChatMessage>>() {
            for plain in &to_push {
                vec_model.push(plain.to_slint());
            }
        }
    }
    move_session_to_top(session_id);
}

/// Drain pending messages into the window model (no session routing).
fn flush_messages(pending: Arc<Mutex<VecDeque<PlainChatMessage>>>, weak: &slint::Weak<MainWindow>) {
    let Some(win) = weak.upgrade() else { return };
    let mut queue = pending.lock().unwrap();
    if queue.is_empty() {
        return;
    }
    let msgs_rc = win.get_messages();
    if let Some(vec_model) = msgs_rc.as_any().downcast_ref::<VecModel<ChatMessage>>() {
        while let Some(plain) = queue.pop_front() {
            vec_model.push(plain.to_slint());
        }
    }
}

/// Drain pending toasts into the window toast model.
fn flush_toasts(pending: Arc<Mutex<VecDeque<PlainToast>>>, weak: &slint::Weak<MainWindow>) {
    let Some(win) = weak.upgrade() else { return };
    let mut queue = pending.lock().unwrap();
    let toasts_rc = win.get_toasts();
    if let Some(vec_model) = toasts_rc.as_any().downcast_ref::<VecModel<ToastItem>>() {
        while let Some(t) = queue.pop_front() {
            vec_model.push(ToastItem {
                message: SharedString::from(t.message),
                level: SharedString::from(t.level),
                dismiss_after_ms: 5000,
            });
            while vec_model.row_count() > 5 {
                vec_model.remove(0);
            }
        }
    }
}
