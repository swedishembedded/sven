// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between sven-frontend async agent events and the Slint UI model.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, VecModel};
use sven_config::{AgentMode, ModelConfig};
use sven_core::AgentEvent;
use sven_frontend::{
    agent_task,
    commands::{CommandContext, CommandRegistry, ImmediateAction, ParsedCommand},
    markdown::{parse_markdown_blocks, MarkdownBlock},
    node_agent_task,
    queue::QueueState,
    tool_view::extract_tool_view,
    AgentRequest, NodeBackend, QueuedMessage,
};
use sven_model::catalog;
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::{
    ChatMessage, CodeLine, CodeToken, CompletionEntry, MainWindow, PickerItem, SessionItem,
    ToastItem,
};

// ── Syntax highlighting ───────────────────────────────────────────────────────

/// Per-token highlight data: text + RGB color.  Send-safe (no Slint types).
type HighlightToken = (String, u8, u8, u8);

/// Highlight `code` using syntect. Returns one Vec<HighlightToken> per line.
/// Falls back to a single plain line if the language is unknown.
fn highlight_code(language: &str, code: &str) -> Vec<Vec<HighlightToken>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::ThemeSet;
    use syntect::parsing::SyntaxSet;
    use syntect::util::LinesWithEndings;

    let ss = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-ocean.dark"];

    let syntax = ss
        .find_syntax_by_token(language)
        .or_else(|| ss.find_syntax_by_extension(language))
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut h = HighlightLines::new(syntax, theme);
    let mut result: Vec<Vec<HighlightToken>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = match h.highlight_line(line, &ss) {
            Ok(r) => r,
            Err(_) => {
                result.push(vec![(
                    line.trim_end_matches('\n').to_string(),
                    0xa5,
                    0xd6,
                    0xff,
                )]);
                continue;
            }
        };

        let tokens: Vec<HighlightToken> = ranges
            .iter()
            .filter_map(|(style, text)| {
                let t = text.trim_end_matches('\n');
                if t.is_empty() {
                    None
                } else {
                    Some((
                        t.to_string(),
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    ))
                }
            })
            .collect();

        // Always push the line (even if empty, to preserve line count)
        result.push(tokens);
    }

    result
}

/// Strip common inline markdown markers for live-streaming preview.
/// Removes `**`, `__`, `*`, `_`, `` ` ``, `##` leaders, etc.
/// Does NOT try to parse structure — just cleans up the most visually noisy
/// cases so that partial streaming text doesn't contain raw asterisks.
fn strip_inline_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let trimmed = line.trim_start_matches('#').trim_start();
        // Strip bold/italic markers
        let cleaned = trimmed
            .replace("**", "")
            .replace("__", "")
            .replace("~~", "");
        // Strip lone `*` and `_` used as emphasis (simple heuristic: only
        // when surrounded by spaces or at word boundaries)
        let cleaned = cleaned
            .replace(" *", " ")
            .replace("* ", " ")
            .replace(" _", " ")
            .replace("_ ", " ");
        out.push_str(&cleaned);
        out.push('\n');
    }
    out.trim_end().to_string()
}

// ── Plain-data structs for cross-thread communication ─────────────────────────

#[derive(Clone, Default)]
struct PlainChatMessage {
    message_type: &'static str,
    content: String,
    role: &'static str,

    is_first_in_group: bool,
    is_error: bool,
    is_streaming: bool,
    is_expanded: bool,

    tool_name: String,
    tool_icon: String,
    tool_summary: String,
    tool_category: String,
    tool_fields_json: String,

    language: String,
    heading_level: i32,

    /// Syntax-highlighted lines: each line is a list of (text, r, g, b) tokens.
    code_lines: Vec<Vec<HighlightToken>>,
}

impl PlainChatMessage {
    fn user(content: impl Into<String>) -> Self {
        Self {
            message_type: "user",
            content: content.into(),
            role: "user",
            ..Default::default()
        }
    }

    fn system(content: impl Into<String>) -> Self {
        Self {
            message_type: "system",
            content: content.into(),
            role: "system",
            ..Default::default()
        }
    }

    fn error(content: impl Into<String>) -> Self {
        Self {
            message_type: "error",
            content: content.into(),
            role: "error",
            is_error: true,
            ..Default::default()
        }
    }

    fn to_slint(&self) -> ChatMessage {
        let code_lines_model = if self.code_lines.is_empty() {
            ModelRc::new(VecModel::<CodeLine>::default())
        } else {
            let lines: Vec<CodeLine> = self
                .code_lines
                .iter()
                .map(|line| {
                    let tokens: Vec<CodeToken> = line
                        .iter()
                        .map(|(text, r, g, b)| CodeToken {
                            text: SharedString::from(text.as_str()),
                            color: Color::from_rgb_u8(*r, *g, *b),
                        })
                        .collect();
                    CodeLine {
                        tokens: ModelRc::new(VecModel::from(tokens)),
                    }
                })
                .collect();
            ModelRc::new(VecModel::from(lines))
        };

        ChatMessage {
            message_type: SharedString::from(self.message_type),
            content: SharedString::from(self.content.as_str()),
            role: SharedString::from(self.role),
            is_first_in_group: self.is_first_in_group,
            is_error: self.is_error,
            is_streaming: self.is_streaming,
            is_expanded: self.is_expanded,
            tool_name: SharedString::from(self.tool_name.as_str()),
            tool_icon: SharedString::from(self.tool_icon.as_str()),
            tool_summary: SharedString::from(self.tool_summary.as_str()),
            tool_category: SharedString::from(self.tool_category.as_str()),
            tool_fields_json: SharedString::from(self.tool_fields_json.as_str()),
            language: SharedString::from(self.language.as_str()),
            heading_level: self.heading_level,
            code_lines: code_lines_model,
        }
    }
}

#[derive(Clone)]
struct PlainToast {
    message: String,
    level: &'static str,
}

/// Converts markdown text into a sequence of PlainChatMessages (one per block).
/// Code blocks are syntax-highlighted via syntect.
fn markdown_to_plain_messages(text: &str, role: &'static str) -> Vec<PlainChatMessage> {
    let blocks = parse_markdown_blocks(text);
    if blocks.is_empty() {
        return vec![PlainChatMessage {
            message_type: "assistant",
            content: text.to_string(),
            role,
            is_first_in_group: true,
            ..Default::default()
        }];
    }

    let mut messages: Vec<PlainChatMessage> = Vec::with_capacity(blocks.len());
    let mut is_first = true;

    for block in blocks {
        let msg = match block {
            MarkdownBlock::Paragraph(text) => PlainChatMessage {
                message_type: "assistant",
                content: text,
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::Heading { level, text } => PlainChatMessage {
                message_type: "heading",
                content: text,
                role,
                is_first_in_group: is_first,
                heading_level: level as i32,
                ..Default::default()
            },
            MarkdownBlock::CodeBlock { language, code } => {
                let code_lines = highlight_code(&language, &code);
                PlainChatMessage {
                    message_type: "code-block",
                    content: code,
                    role,
                    is_first_in_group: is_first,
                    language,
                    code_lines,
                    ..Default::default()
                }
            }
            MarkdownBlock::ListItem { depth, text } => PlainChatMessage {
                message_type: "list-item",
                content: text,
                role,
                is_first_in_group: is_first,
                heading_level: depth as i32,
                ..Default::default()
            },
            MarkdownBlock::Separator => PlainChatMessage {
                message_type: "separator",
                content: String::new(),
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::BlockQuote(text) => PlainChatMessage {
                message_type: "block-quote",
                content: text,
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::InlineCode(text) => PlainChatMessage {
                message_type: "assistant",
                content: format!("`{text}`"),
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
            MarkdownBlock::TableRow(cells) => PlainChatMessage {
                message_type: "assistant",
                content: cells.join(" │ "),
                role,
                is_first_in_group: is_first,
                ..Default::default()
            },
        };
        is_first = false;
        messages.push(msg);
    }

    messages
}

// ── Options ───────────────────────────────────────────────────────────────────

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
    /// Build the app (creates window, spawns agent task).
    pub async fn build(opts: SvenAppOptions) -> anyhow::Result<Self> {
        let window = MainWindow::new()?;

        // ── Initial session ───────────────────────────────────────────────────
        let initial_session_id = "session-0".to_string();
        let sessions_model = Rc::new(VecModel::<SessionItem>::default());
        sessions_model.push(SessionItem {
            id: SharedString::from(&initial_session_id),
            title: SharedString::from("New chat"),
            busy: false,
            active: true,
            depth: 0,
            status: SharedString::from("active"),
            current_tool: SharedString::new(),
            total_cost_usd: 0.0,
        });
        window.set_sessions(ModelRc::from(sessions_model.clone()));
        window.set_active_session_id(SharedString::from(&initial_session_id));

        let msgs_model = Rc::new(VecModel::<ChatMessage>::default());
        window.set_messages(ModelRc::from(msgs_model.clone()));
        window.set_toasts(ModelRc::new(VecModel::<ToastItem>::default()));
        window.set_model_name(SharedString::from(&opts.model_cfg.name));
        window.set_mode(SharedString::from(
            format!("{:?}", opts.mode).to_lowercase(),
        ));

        // ── Per-session message store ─────────────────────────────────────────
        // Maps session_id → Vec of committed messages for that session.
        let session_messages: Arc<Mutex<HashMap<String, Vec<PlainChatMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let active_session_id: Arc<Mutex<String>> =
            Arc::new(Mutex::new(initial_session_id.clone()));

        // ── Current model/mode tracking ───────────────────────────────────────
        let current_mode = Arc::new(Mutex::new(opts.mode));
        let current_model_name = Arc::new(Mutex::new(opts.model_cfg.name.clone()));
        let current_model_provider = Arc::new(Mutex::new(opts.model_cfg.provider.clone()));

        // ── Agent channels ────────────────────────────────────────────────────
        let (agent_tx, agent_rx) = mpsc::channel::<AgentRequest>(64);
        let (event_tx, mut event_rx) = mpsc::channel::<AgentEvent>(256);
        let (question_tx, _question_rx) = mpsc::channel::<QuestionRequest>(16);
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

        // ── Shared state for event bridge ─────────────────────────────────────
        let pending_msgs: Arc<Mutex<VecDeque<PlainChatMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let pending_toasts: Arc<Mutex<VecDeque<PlainToast>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let streaming_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let thinking_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let total_cost = Arc::new(Mutex::new(0.0f64));
        let total_output_tokens = Arc::new(Mutex::new(0u32));
        let total_input_tokens = Arc::new(Mutex::new(0u32));

        let queue_state: Arc<Mutex<QueueState>> = Arc::new(Mutex::new(QueueState::new()));
        let is_first_message: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));

        let weak = window.as_weak();

        // ── Toggle thinking/tool expand ───────────────────────────────────────
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

        // ── Send message callback ─────────────────────────────────────────────
        {
            let tx = agent_tx.clone();
            let tx2 = agent_tx.clone();
            let pm_send = Arc::clone(&pending_msgs);
            let weak_send = weak.clone();
            let is_first = Arc::clone(&is_first_message);
            let queue = Arc::clone(&queue_state);
            let cur_mode = Arc::clone(&current_mode);
            let cur_provider = Arc::clone(&current_model_provider);
            let cur_model = Arc::clone(&current_model_name);
            let config = Arc::clone(&opts.config);
            let sb_send = Arc::clone(&streaming_buf);
            let tb_send = Arc::clone(&thinking_buf);
            let cancel_handle_sm = Arc::clone(&cancel_handle);
            window.on_send_message(move |text| {
                let content = text.to_string();
                if content.is_empty() {
                    return;
                }

                // Show the user message immediately in the chat
                pm_send
                    .lock()
                    .unwrap()
                    .push_back(PlainChatMessage::user(&content));
                let _ = slint::invoke_from_event_loop({
                    let pm2 = Arc::clone(&pm_send);
                    let w = weak_send.clone();
                    move || flush_messages(pm2, &w)
                });

                let agent_busy = weak_send
                    .upgrade()
                    .map(|w| w.get_agent_busy())
                    .unwrap_or(false);

                // ── Slash command dispatch ────────────────────────────────────
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
                        // Handle immediate actions
                        if let Some(action) = result.immediate_action {
                            let w = weak_send.clone();
                            let pm3 = Arc::clone(&pm_send);
                            let sb3 = Arc::clone(&sb_send);
                            let tb3 = Arc::clone(&tb_send);
                            let ch3 = Arc::clone(&cancel_handle_sm);
                            match action {
                                ImmediateAction::ClearChat => {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        *sb3.lock().unwrap() = String::new();
                                        *tb3.lock().unwrap() = String::new();
                                        pm3.lock().unwrap().clear();
                                        if let Some(win) = w.upgrade() {
                                            // Get model from window on UI thread — safe (Rc stays on UI thread)
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
                                _ => {}
                            }
                        }

                        // If the command also sends a message, queue or submit it
                        if let Some(msg) = result.message_to_send {
                            if agent_busy {
                                queue.lock().unwrap().push(QueuedMessage {
                                    content: msg,
                                    model_transition: None,
                                    mode_transition: result.mode_override,
                                });
                            } else {
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
                            // Model/mode override — stage for next send
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

                // ── Enqueue while busy ────────────────────────────────────────
                if agent_busy {
                    queue.lock().unwrap().push(QueuedMessage {
                        content: content.clone(),
                        model_transition: None,
                        mode_transition: None,
                    });
                    return;
                }

                // ── Generate title on first real message ──────────────────────
                let gen_title = {
                    let mut first = is_first.lock().unwrap();
                    if *first {
                        *first = false;
                        true
                    } else {
                        false
                    }
                };

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

        // ── New session ───────────────────────────────────────────────────────
        {
            let msgs_ns = Rc::clone(&msgs_model);
            let sessions_ns = Rc::clone(&sessions_model);
            let is_first_ns = Arc::clone(&is_first_message);
            let queue_ns = Arc::clone(&queue_state);
            let sb_ns = Arc::clone(&streaming_buf);
            let tb_ns = Arc::clone(&thinking_buf);
            let weak_ns = weak.clone();
            let session_msgs_ns = Arc::clone(&session_messages);
            let active_sid_ns = Arc::clone(&active_session_id);

            window.on_new_session(move || {
                // Save current session messages before switching
                let current_id = active_sid_ns.lock().unwrap().clone();
                {
                    let current_msgs: Vec<PlainChatMessage> = (0..msgs_ns.row_count())
                        .filter_map(|i| msgs_ns.row_data(i))
                        .map(|m| PlainChatMessage {
                            message_type: "system",
                            content: m.content.to_string(),
                            ..Default::default()
                        })
                        .collect();
                    session_msgs_ns
                        .lock()
                        .unwrap()
                        .insert(current_id, current_msgs);
                }

                // Clear messages
                while msgs_ns.row_count() > 0 {
                    msgs_ns.remove(0);
                }

                *sb_ns.lock().unwrap() = String::new();
                *tb_ns.lock().unwrap() = String::new();
                *queue_ns.lock().unwrap() = QueueState::new();
                *is_first_ns.lock().unwrap() = true;

                let count = sessions_ns.row_count();
                let new_id = format!("session-{count}");

                // Deactivate all sessions
                for i in 0..sessions_ns.row_count() {
                    if let Some(mut s) = sessions_ns.row_data(i) {
                        s.active = false;
                        sessions_ns.set_row_data(i, s);
                    }
                }

                sessions_ns.push(SessionItem {
                    id: SharedString::from(new_id.clone()),
                    title: SharedString::from("New chat"),
                    busy: false,
                    active: true,
                    depth: 0,
                    status: SharedString::from("active"),
                    current_tool: SharedString::new(),
                    total_cost_usd: 0.0,
                });

                *active_sid_ns.lock().unwrap() = new_id.clone();

                if let Some(win) = weak_ns.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id));
                    win.set_streaming_text(SharedString::new());
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                }
            });
        }

        // ── Session selected ──────────────────────────────────────────────────
        {
            let msgs_ss = Rc::clone(&msgs_model);
            let sessions_ss = Rc::clone(&sessions_model);
            let weak_ss = weak.clone();
            let session_msgs_ss = Arc::clone(&session_messages);
            let active_sid_ss = Arc::clone(&active_session_id);
            let sb_ss = Arc::clone(&streaming_buf);
            let tb_ss = Arc::clone(&thinking_buf);

            window.on_session_selected(move |id| {
                let new_id = id.to_string();
                let current_id = active_sid_ss.lock().unwrap().clone();
                if new_id == current_id {
                    return;
                }

                // Save current session's messages
                {
                    let snaps: Vec<PlainChatMessage> = (0..msgs_ss.row_count())
                        .filter_map(|i| msgs_ss.row_data(i))
                        .map(|m| slint_msg_to_plain(&m))
                        .collect();
                    session_msgs_ss.lock().unwrap().insert(current_id, snaps);
                }

                // Switch active marker
                for i in 0..sessions_ss.row_count() {
                    if let Some(mut s) = sessions_ss.row_data(i) {
                        s.active = s.id == id;
                        sessions_ss.set_row_data(i, s);
                    }
                }

                *active_sid_ss.lock().unwrap() = new_id.clone();

                // Load selected session's messages
                let saved = session_msgs_ss
                    .lock()
                    .unwrap()
                    .get(&new_id)
                    .cloned()
                    .unwrap_or_default();

                while msgs_ss.row_count() > 0 {
                    msgs_ss.remove(0);
                }
                for m in &saved {
                    msgs_ss.push(m.to_slint());
                }

                *sb_ss.lock().unwrap() = String::new();
                *tb_ss.lock().unwrap() = String::new();

                if let Some(win) = weak_ss.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id));
                    win.set_streaming_text(SharedString::new());
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                }
            });
        }

        window.on_question_answered(|_| {});
        window.on_question_dismissed(|| {});

        // ── Slash command completion ───────────────────────────────────────────
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
                }
            });
        }

        // ── Completion accepted ────────────────────────────────────────────────
        {
            let weak_ca = weak.clone();
            window.on_completion_accepted(move |val| {
                let Some(win) = weak_ca.upgrade() else { return };
                let val_str = val.to_string();

                // Empty val = "accept selected" signal from Tab key
                let apply_val = if val_str.is_empty() {
                    // Read the currently-selected completion item
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
                    win.set_input_text(SharedString::from(new_text));
                }
                // Clear completions
                win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
            });
        }

        // ── Picker all-items store (must be declared before model/mode clicked) ──
        let picker_all_items: Arc<Mutex<Vec<PickerItem>>> = Arc::new(Mutex::new(Vec::new()));
        let picker_all_for_model: Arc<Mutex<Vec<PickerItem>>> = Arc::clone(&picker_all_items);
        let picker_all_for_mode: Arc<Mutex<Vec<PickerItem>>> = Arc::clone(&picker_all_items);

        // ── Model clicked ─────────────────────────────────────────────────────
        {
            let weak_mc = weak.clone();
            let config_mc = Arc::clone(&opts.config);
            let cur_provider_mc = Arc::clone(&current_model_provider);
            let cur_model_mc = Arc::clone(&current_model_name);
            let all_mc = Arc::clone(&picker_all_for_model);

            window.on_model_clicked(move || {
                let current = format!(
                    "{}/{}",
                    cur_provider_mc.lock().unwrap(),
                    cur_model_mc.lock().unwrap()
                );

                let mut items: Vec<PickerItem> = Vec::new();
                items.push(PickerItem {
                    id: SharedString::from(current.clone()),
                    label: SharedString::from(format!("{} ✓", current)),
                    description: SharedString::from("current model"),
                });

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
                }
            });
        }

        // ── Mode clicked ──────────────────────────────────────────────────────
        {
            let weak_mc = weak.clone();
            let all_mc = Arc::clone(&picker_all_for_mode);
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
                if let Some(win) = weak_mc.upgrade() {
                    win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(items))));
                    win.set_picker_title(SharedString::from("Switch mode"));
                    win.set_picker_visible(true);
                }
            });
        }

        // ── Picker selected ───────────────────────────────────────────────────
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

                if title.contains("mode") {
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

        // ── Picker dismissed ──────────────────────────────────────────────────
        {
            let weak_pd = weak.clone();
            window.on_picker_dismissed(move || {
                if let Some(win) = weak_pd.upgrade() {
                    win.set_picker_visible(false);
                }
            });
        }

        // ── Picker search (fuzzy filter from Rust) ────────────────────────────
        {
            let weak_psc = weak.clone();
            let all = Arc::clone(&picker_all_items);
            window.on_picker_search_changed(move |query| {
                let Some(win) = weak_psc.upgrade() else {
                    return;
                };
                let query_lower = query.to_string().to_lowercase();
                let all_items = all.lock().unwrap();
                if query_lower.is_empty() {
                    win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(all_items.clone()))));
                    return;
                }
                let filtered: Vec<PickerItem> = all_items
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
                    .collect();
                win.set_picker_items(ModelRc::from(Rc::new(VecModel::from(filtered))));
            });
        }

        // ── Event bridge ──────────────────────────────────────────────────────
        let pm = Arc::clone(&pending_msgs);
        let pt = Arc::clone(&pending_toasts);
        let sb = Arc::clone(&streaming_buf);
        let tb = Arc::clone(&thinking_buf);
        let tc = Arc::clone(&total_cost);
        let tt_out = Arc::clone(&total_output_tokens);
        let tt_in = Arc::clone(&total_input_tokens);
        let queue_ev = Arc::clone(&queue_state);
        let tx_ev = agent_tx.clone();
        let weak2 = weak.clone();

        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                match event {
                    AgentEvent::TextDelta(delta) => {
                        let mut buf = sb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = strip_inline_markdown(&buf);
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            move || {
                                if let Some(win) = w.upgrade() {
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
                        let msgs = markdown_to_plain_messages(&text, "assistant");
                        {
                            let mut q = pm.lock().unwrap();
                            for m in msgs {
                                q.push_back(m);
                            }
                        }
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

                    AgentEvent::ThinkingDelta(delta) => {
                        let mut buf = tb.lock().unwrap();
                        buf.push_str(&delta);
                        let text = strip_inline_markdown(&buf);
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
                        *tb.lock().unwrap() = String::new();
                        // Strip markdown for the stored thinking content
                        let stripped = strip_inline_markdown(&content);
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "thinking",
                            content: stripped,
                            role: "thinking",
                            is_first_in_group: false,
                            is_expanded: false,
                            ..Default::default()
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
                        let view = extract_tool_view(&tc_call.name, &tc_call.args, None);
                        let fields_json = format_fields_json(&view.fields);
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "tool-call",
                            content: tc_call.args.to_string(),
                            role: "assistant",
                            tool_name: tc_call.name,
                            tool_icon: view.icon,
                            tool_summary: view.summary,
                            tool_category: view.category,
                            tool_fields_json: fields_json,
                            is_expanded: false,
                            ..Default::default()
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
                            role: "tool",
                            is_error,
                            // Auto-expand error results so the error text is visible
                            is_expanded: is_error,
                            ..Default::default()
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

                        let next = queue_ev.lock().unwrap().pop_front();
                        let queue_len = queue_ev.lock().unwrap().len();

                        if let Some(queued) = next {
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
                        }

                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            let queue_empty = queue_len == 0;
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_streaming_text(SharedString::new());
                                    win.set_thinking_text(SharedString::new());
                                    if queue_empty {
                                        win.set_agent_busy(false);
                                    }
                                    win.set_queue_count(queue_len as i32);
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
                        pm.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::error(&err_msg));
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
                            let mut t = tt_out.lock().unwrap();
                            *t = t.saturating_add(output);
                        }
                        if input > 0 {
                            let mut t = tt_in.lock().unwrap();
                            *t = t.saturating_add(input);
                        }
                        if let Some(c) = cost_usd {
                            *tc.lock().unwrap() += c;
                        }
                        let cost_f32 = *tc.lock().unwrap() as f32;
                        let out_tokens = *tt_out.lock().unwrap() as i32;
                        let in_tokens = *tt_in.lock().unwrap() as i32;
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
                            let title = title.clone();
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
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
                        });
                    }

                    AgentEvent::CollabEvent(ev) => {
                        let text = sven_core::prompts::format_collab_event(&ev);
                        pm.lock().unwrap().push_back(PlainChatMessage::system(text));
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
                        pm.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::system(format!(
                            "Delegated \"{task_title}\" to {to_name}: {status} — {result_preview}"
                        )));
                        let _ = slint::invoke_from_event_loop({
                            let pm2 = Arc::clone(&pm);
                            let w = weak2.clone();
                            move || flush_messages(pm2, &w)
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

/// Convert a Slint ChatMessage back to a PlainChatMessage for session save/restore.
/// The `message_type` and `role` are stored in the `content` field of a special
/// "session-snapshot" envelope. On restore, `to_slint()` re-inflates the original
/// Slint struct by serialising all fields into the plain struct's content.
fn slint_msg_to_plain(m: &ChatMessage) -> PlainChatMessage {
    PlainChatMessage {
        // Preserve the message_type by mapping back from the SharedString
        message_type: match m.message_type.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            "code-block" => "code-block",
            "heading" => "heading",
            "list-item" => "list-item",
            "block-quote" => "block-quote",
            "separator" => "separator",
            "thinking" => "thinking",
            "tool-call" => "tool-call",
            "tool-result" => "tool-result",
            "error" => "error",
            _ => "system",
        },
        content: m.content.to_string(),
        role: "user", // will be overridden below
        is_first_in_group: m.is_first_in_group,
        is_error: m.is_error,
        is_expanded: m.is_expanded,
        tool_name: m.tool_name.to_string(),
        tool_icon: m.tool_icon.to_string(),
        tool_summary: m.tool_summary.to_string(),
        tool_category: m.tool_category.to_string(),
        tool_fields_json: m.tool_fields_json.to_string(),
        language: m.language.to_string(),
        heading_level: m.heading_level,
        ..Default::default()
    }
}

/// Format tool fields as a readable multi-line string.
fn format_fields_json(fields: &[(String, String)]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    fields
        .iter()
        .map(|(k, v)| {
            let v_short: String = v.chars().take(120).collect();
            let v_display = if v.len() > 120 {
                format!("{v_short}…")
            } else {
                v_short
            };
            format!("{k}: {v_display}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drain pending messages into the window model (Slint main thread only).
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

/// Drain pending toasts into the window model (Slint main thread only).
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
            while vec_model.row_count() > 5 {
                vec_model.remove(0);
            }
        }
    }
}
