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
use sven_input::{
    chat_path, ensure_chat_dir, json_str_to_yaml, list_chats, load_chat_from, save_chat_to,
    yaml_to_json_str, ChatDocument, ChatStatus, SessionId, TurnRecord,
};
use sven_model::catalog;
use sven_model::{FunctionCall, Message as SvenMessage, MessageContent, Role};
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays, TodoItem};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::{
    ChatMessage, CodeLine, CodeToken, CompletionEntry, MainWindow, PickerItem, QuestionItem,
    QueueItem, SessionItem, ToastItem,
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

/// Convert a ChatDocument's turns to PlainChatMessages for display.
fn chat_document_to_plain_messages(doc: &ChatDocument) -> Vec<PlainChatMessage> {
    let mut out = Vec::new();
    for turn in &doc.turns {
        match turn {
            TurnRecord::User { content } => {
                out.push(PlainChatMessage::user(content));
            }
            TurnRecord::Assistant { content } => {
                out.extend(markdown_to_plain_messages(content, "assistant"));
            }
            TurnRecord::Thinking { content } => {
                out.push(PlainChatMessage {
                    message_type: "thinking",
                    content: content.clone(),
                    role: "thinking",
                    is_first_in_group: false,
                    ..Default::default()
                });
            }
            TurnRecord::ToolCall {
                tool_call_id: _,
                name,
                arguments,
            } => {
                let args_json = yaml_to_json_str(arguments);
                let args_value: serde_json::Value = serde_json::from_str(&args_json)
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let view = extract_tool_view(name, &args_value, None);
                let fields_json = format_fields_json(&view.fields);
                out.push(PlainChatMessage {
                    message_type: "tool-call",
                    content: args_json,
                    role: "assistant",
                    tool_name: name.clone(),
                    tool_icon: view.icon,
                    tool_summary: view.summary,
                    tool_category: view.category,
                    tool_fields_json: fields_json,
                    is_expanded: false,
                    ..Default::default()
                });
            }
            TurnRecord::ToolResult {
                tool_call_id: _,
                content,
            } => {
                let preview: String = content.chars().take(500).collect();
                out.push(PlainChatMessage {
                    message_type: "tool-result",
                    content: preview,
                    role: "tool",
                    ..Default::default()
                });
            }
            TurnRecord::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy,
                ..
            } => {
                let strat = strategy.as_deref().unwrap_or("unknown");
                out.push(PlainChatMessage::system(format!(
                    "Context compacted ({strat}): {tokens_before}→{tokens_after} tokens"
                )));
            }
        }
    }
    out
}

/// Convert PlainChatMessage slice to TurnRecords for saving to ChatDocument.
/// Merges consecutive assistant display blocks into single assistant turns.
fn plain_messages_to_turns(plain: &[PlainChatMessage]) -> Vec<TurnRecord> {
    let mut turns = Vec::new();
    let mut assistant_buf = String::new();
    let mut tool_call_counter = 0u32;
    let mut last_tool_call_id: Option<String> = None;

    for p in plain {
        match p.message_type {
            "user" => {
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                turns.push(TurnRecord::User {
                    content: p.content.clone(),
                });
            }
            "assistant" | "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row" => {
                if !assistant_buf.is_empty() {
                    assistant_buf.push('\n');
                }
                assistant_buf.push_str(&p.content);
            }
            "tool-call" => {
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                let id = format!("call_{}", tool_call_counter);
                tool_call_counter += 1;
                last_tool_call_id = Some(id.clone());
                let arguments = json_str_to_yaml(&p.content);
                turns.push(TurnRecord::ToolCall {
                    tool_call_id: id,
                    name: p.tool_name.clone(),
                    arguments,
                });
            }
            "tool-result" => {
                if let Some(id) = last_tool_call_id.take() {
                    turns.push(TurnRecord::ToolResult {
                        tool_call_id: id,
                        content: p.content.clone(),
                    });
                }
            }
            "thinking" => {
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                turns.push(TurnRecord::Thinking {
                    content: p.content.clone(),
                });
            }
            "system" => {
                if !assistant_buf.is_empty() {
                    turns.push(TurnRecord::Assistant {
                        content: std::mem::take(&mut assistant_buf),
                    });
                }
                if let Some((tb, ta, strat)) = parse_context_compacted(&p.content) {
                    turns.push(TurnRecord::ContextCompacted {
                        tokens_before: tb,
                        tokens_after: ta,
                        strategy: Some(strat),
                        turn: None,
                    });
                }
            }
            _ => {}
        }
    }
    if !assistant_buf.is_empty() {
        turns.push(TurnRecord::Assistant {
            content: assistant_buf,
        });
    }
    turns
}

/// Parse "Context compacted (X): N→M tokens" into (before, after, strategy).
fn parse_context_compacted(s: &str) -> Option<(usize, usize, String)> {
    let rest = s.strip_prefix("Context compacted (")?;
    let (strat, rest) = rest.split_once("): ")?;
    let (before_str, after_str) = rest.split_once("→")?;
    let before = before_str.trim().parse().ok()?;
    let after_clean = after_str.trim().trim_end_matches(" tokens");
    let after = after_clean.parse().ok()?;
    Some((before, after, strat.to_string()))
}

/// Save a session's messages to disk (same format as TUI).
fn save_session_to_disk(
    session_id: &str,
    plain: &[PlainChatMessage],
    title: &str,
    model: Option<&str>,
    mode: Option<&str>,
) {
    let turns = plain_messages_to_turns(plain);
    if turns.is_empty() {
        return;
    }
    let sid = SessionId::from_string(session_id.to_string());
    let path = chat_path(&sid);
    if let Err(e) = ensure_chat_dir() {
        tracing::warn!("cannot create chat dir: {e}");
        return;
    }
    let mut doc = ChatDocument {
        id: sid,
        title: title.to_string(),
        model: model.map(String::from),
        mode: mode.map(String::from),
        status: ChatStatus::Active,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        parent_id: None,
        turns,
    };
    if let Err(e) = save_chat_to(&path, &mut doc) {
        tracing::warn!("failed to save chat {}: {e}", path.display());
    }
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
            current_tool: SharedString::new(),
            total_cost_usd: 0.0,
        });
        // Load chat history from disk (same as TUI) and add to sidebar
        if let Ok(entries) = list_chats(Some(50)) {
            for chat_entry in &entries {
                let id_str = chat_entry.id.as_str().to_string();
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
                    current_tool: SharedString::new(),
                    total_cost_usd: 0.0,
                });
            }
        }

        window.set_sessions(ModelRc::from(sessions_model.clone()));
        window.set_active_session_id(SharedString::from(&initial_session_id_str));

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

        // ── Per-session message store ─────────────────────────────────────────
        // Maps session_id → Vec of committed messages for that session.
        let session_messages: Arc<Mutex<HashMap<String, Vec<PlainChatMessage>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let active_session_id: Arc<Mutex<String>> =
            Arc::new(Mutex::new(initial_session_id_str.clone()));

        // ── Current model/mode tracking ───────────────────────────────────────
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
        let editing_msg_index: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
        let is_first_message: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
        // Session that owns the currently running agent (streaming). Events are routed here.
        let streaming_session_id: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        // Current todo list per session (updated by TodoUpdate; used for todo tool-call display).
        let current_todos: Arc<Mutex<HashMap<String, Vec<TodoItem>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let weak = window.as_weak();

        // ── Message edit (restart from here) ───────────────────────────────────
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
            window.on_send_message(move |text| {
                let content = text.to_string();
                if content.is_empty() {
                    return;
                }

                // ── Edit-and-restart: truncate at edited message, resubmit ─────
                if let Some(idx) = editing_send.lock().unwrap().take() {
                    let mut plain: Vec<PlainChatMessage> = (0..idx)
                        .filter_map(|i| msgs_send.row_data(i))
                        .map(|m| slint_msg_to_plain(&m))
                        .collect();
                    plain.push(PlainChatMessage::user(&content));
                    let messages = plain_messages_to_sven_messages(&plain);
                    let sid = active_sid_send.lock().unwrap().clone();
                    // Truncate display at idx, replace with edited
                    while msgs_send.row_count() > idx + 1 {
                        msgs_send.remove(idx + 1);
                    }
                    if let Some(mut row) = msgs_send.row_data(idx) {
                        row.content = SharedString::from(&content);
                        msgs_send.set_row_data(idx, row);
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
                                    let pt = Arc::clone(&pt_send);
                                    let w = weak_send.clone();
                                    pt.lock().unwrap().push_back(PlainToast {
                                        message: "Team picker not yet available in GUI".to_string(),
                                        level: "info",
                                    });
                                    let _ = slint::invoke_from_event_loop(move || {
                                        flush_toasts(pt, &w);
                                    });
                                }
                                ImmediateAction::OpenInspector { kind } => {
                                    let pt = Arc::clone(&pt_send);
                                    let w = weak_send.clone();
                                    let title = kind.title().to_string();
                                    pt.lock().unwrap().push_back(PlainToast {
                                        message: format!(
                                            "Inspector ({title}) not yet available in GUI"
                                        ),
                                        level: "info",
                                    });
                                    let _ = slint::invoke_from_event_loop(move || {
                                        flush_toasts(pt, &w);
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
                                let items: Vec<QueueItem> = queue
                                    .lock()
                                    .unwrap()
                                    .messages
                                    .iter()
                                    .enumerate()
                                    .map(|(i, qm)| QueueItem {
                                        index: i as i32,
                                        content: SharedString::from(
                                            qm.content
                                                .lines()
                                                .next()
                                                .unwrap_or("")
                                                .chars()
                                                .take(80)
                                                .collect::<String>(),
                                        ),
                                    })
                                    .collect();
                                let qlen = items.len();
                                let w = weak_send.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(win) = w.upgrade() {
                                        win.set_queue_items(ModelRc::new(VecModel::from(items)));
                                        win.set_queue_count(qlen as i32);
                                    }
                                });
                            } else {
                                pm_send
                                    .lock()
                                    .unwrap()
                                    .push_back(PlainChatMessage::user(&msg));
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

                // ── Enqueue while busy (do NOT add to chat — queue panel shows it) ─
                if agent_busy {
                    queue.lock().unwrap().push(QueuedMessage {
                        content: content.clone(),
                        model_transition: None,
                        mode_transition: None,
                    });
                    let items: Vec<QueueItem> = queue
                        .lock()
                        .unwrap()
                        .messages
                        .iter()
                        .enumerate()
                        .map(|(i, qm)| QueueItem {
                            index: i as i32,
                            content: SharedString::from(
                                qm.content
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(80)
                                    .collect::<String>(),
                            ),
                        })
                        .collect();
                    let qlen = items.len();
                    queue_items_send.clear();
                    for item in &items {
                        queue_items_send.push(item.clone());
                    }
                    if let Some(win) = weak_send.upgrade() {
                        win.set_queue_count(qlen as i32);
                    }
                    return;
                }

                // ── Send immediately: add user message to chat, then submit ─────
                pm_send
                    .lock()
                    .unwrap()
                    .push_back(PlainChatMessage::user(&content));
                let sid = active_sid_send.lock().unwrap().clone();
                let _ = slint::invoke_from_event_loop({
                    let pm2 = Arc::clone(&pm_send);
                    let sm = Arc::clone(&session_msgs_send);
                    let w = weak_send.clone();
                    move || flush_messages_to_session(pm2, &sid, sm, &w)
                });

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

        // ── Queue panel actions ────────────────────────────────────────────────
        {
            let tx_q = agent_tx.clone();
            let pm_q = Arc::clone(&pending_msgs);
            let sm_q = Arc::clone(&session_messages);
            let active_q = Arc::clone(&active_session_id);
            let streaming_q = Arc::clone(&streaming_session_id);
            let cur_mode_q = Arc::clone(&current_mode);

            window.on_queue_edit_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move |idx| {
                    let idx = idx as usize;
                    let mut q = queue_q.lock().unwrap();
                    if let Some(qm) = q.messages.get_mut(idx) {
                        let content = qm.content.clone();
                        drop(q);
                        if let Some(win) = weak_q.upgrade() {
                            win.set_input_text(SharedString::from(&content));
                            // Remove from queue and sync
                            let mut q2 = queue_q.lock().unwrap();
                            if idx < q2.messages.len() {
                                q2.messages.remove(idx);
                            }
                            let items: Vec<QueueItem> = q2
                                .messages
                                .iter()
                                .enumerate()
                                .map(|(i, qm)| QueueItem {
                                    index: i as i32,
                                    content: SharedString::from(
                                        qm.content
                                            .lines()
                                            .next()
                                            .unwrap_or("")
                                            .chars()
                                            .take(80)
                                            .collect::<String>(),
                                    ),
                                })
                                .collect();
                            let qlen = items.len();
                            qi_q.clear();
                            for item in items {
                                qi_q.push(item);
                            }
                            win.set_queue_count(qlen as i32);
                        }
                    }
                }
            });

            window.on_queue_delete_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move |idx| {
                    let idx = idx as usize;
                    let mut q = queue_q.lock().unwrap();
                    if idx < q.messages.len() {
                        q.messages.remove(idx);
                        let items: Vec<QueueItem> = q
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(i, qm)| QueueItem {
                                index: i as i32,
                                content: SharedString::from(
                                    qm.content
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .chars()
                                        .take(80)
                                        .collect::<String>(),
                                ),
                            })
                            .collect();
                        let qlen = items.len();
                        drop(q);
                        qi_q.clear();
                        for item in items {
                            qi_q.push(item);
                        }
                        if let Some(win) = weak_q.upgrade() {
                            win.set_queue_count(qlen as i32);
                        }
                    }
                }
            });

            window.on_queue_submit_clicked({
                let queue_q = Arc::clone(&queue_state);
                let weak_q = weak.clone();
                let qi_q = Rc::clone(&queue_items_model);
                move |idx| {
                    let idx = idx as usize;
                    let mut q = queue_q.lock().unwrap();
                    let qm = match q.messages.remove(idx) {
                        Some(m) => m,
                        None => return,
                    };
                    let agent_busy = weak_q
                        .upgrade()
                        .map(|w| w.get_agent_busy())
                        .unwrap_or(false);
                    let items: Vec<QueueItem> = q
                        .messages
                        .iter()
                        .enumerate()
                        .map(|(i, qm)| QueueItem {
                            index: i as i32,
                            content: SharedString::from(
                                qm.content
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .chars()
                                    .take(80)
                                    .collect::<String>(),
                            ),
                        })
                        .collect();
                    let qlen = items.len();
                    qi_q.clear();
                    for item in items {
                        qi_q.push(item);
                    }
                    if let Some(win) = weak_q.upgrade() {
                        win.set_queue_count(qlen as i32);
                    }
                    drop(q);

                    if agent_busy {
                        // Force submit: put at front, abort current run
                        let mut q2 = queue_q.lock().unwrap();
                        q2.messages.push_front(qm);
                        q2.abort_pending = false;
                        drop(q2);
                        let ch = Arc::clone(&cancel_handle);
                        tokio::spawn(async move {
                            if let Some(sender) = ch.lock().await.take() {
                                let _ = sender.send(());
                            }
                        });
                        // Sync queue (we added back at front)
                        let q3 = queue_q.lock().unwrap();
                        let items2: Vec<QueueItem> = q3
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(i, qm)| QueueItem {
                                index: i as i32,
                                content: SharedString::from(
                                    qm.content
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .chars()
                                        .take(80)
                                        .collect::<String>(),
                                ),
                            })
                            .collect();
                        let qlen2 = items2.len();
                        drop(q3);
                        qi_q.clear();
                        for item in items2 {
                            qi_q.push(item);
                        }
                        if let Some(win) = weak_q.upgrade() {
                            win.set_queue_count(qlen2 as i32);
                        }
                    } else {
                        // Agent idle: add to chat and send
                        let content = qm.content.clone();
                        let mode_val = *cur_mode_q.lock().unwrap();
                        pm_q.lock()
                            .unwrap()
                            .push_back(PlainChatMessage::user(&content));
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
            let active_sid_ns = Arc::clone(&active_session_id);

            window.on_new_session(move || {
                // Save current session messages before switching (to memory and disk)
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
                        save_session_to_disk(&current_id, &current_msgs, &title, None, None);
                    }
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
                qi_ns.clear();
                *is_first_ns.lock().unwrap() = true;

                let new_id = SessionId::new().as_str().to_string();

                // Deactivate all sessions
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
                        current_tool: SharedString::new(),
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
                }
            });
        }

        // ── Question modal state (must be before session_selected) ──────────────
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

                // Save current session's messages (to memory and disk)
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
                        save_session_to_disk(
                            &current_id,
                            &snaps,
                            &title,
                            Some(&model),
                            Some(&mode),
                        );
                    }
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

                // Load selected session's messages (from memory or disk)
                let saved = session_msgs_ss.lock().unwrap().get(&new_id).cloned();
                let saved = match saved {
                    Some(msgs) => msgs,
                    None => {
                        // Not in memory — try loading from disk (same chat dir as TUI)
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

                if let Some(win) = weak_ss.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id.clone()));
                    win.set_streaming_text(SharedString::new());
                    win.set_streaming_messages(ModelRc::new(VecModel::<ChatMessage>::default()));
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                }

                // Show pending question if user switched to the session that has it
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

        // ── Question modal (ask_question tool) ─────────────────────────────────
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
                            win.set_question_visible(show);
                        }
                    });
                }
            });
        }

        {
            let pending = Arc::clone(&pending_question);
            let qi_model = Rc::clone(&question_items_model);
            let qidx = Arc::clone(&question_current_index);

            window.on_question_option_selected({
                let qi = Rc::clone(&qi_model);
                move |qidx_val, opt_idx| {
                    let idx = qidx_val as usize;
                    if let Some(mut row) = qi.row_data(idx) {
                        row.selected_index = opt_idx;
                        qi.set_row_data(idx, row);
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
                    win.set_input_text(SharedString::from(&new_text));
                    // Clear completions
                    win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
                    // If result is a complete slash command (e.g. "/agents "), send it immediately
                    if new_text.starts_with('/') && new_text.ends_with(' ') {
                        win.invoke_send_message(SharedString::from(new_text.trim()));
                        win.set_input_text(SharedString::new());
                    }
                } else {
                    win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
                }
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
                            let formatted = format_todos_display(&todos);
                            (formatted, true)
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
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "tool-result",
                            content: preview,
                            role: "tool",
                            is_error,
                            // Auto-expand error results so the error text is visible
                            is_expanded: is_error,
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

                        let items: Vec<QueueItem> = queue_ev
                            .lock()
                            .unwrap()
                            .messages
                            .iter()
                            .enumerate()
                            .map(|(i, qm)| QueueItem {
                                index: i as i32,
                                content: SharedString::from(
                                    qm.content
                                        .lines()
                                        .next()
                                        .unwrap_or("")
                                        .chars()
                                        .take(80)
                                        .collect::<String>(),
                                ),
                            })
                            .collect();
                        let _ = slint::invoke_from_event_loop({
                            let w = weak2.clone();
                            let queue_empty = queue_len == 0;
                            let sm = Arc::clone(&session_msgs_ev);
                            let active = active_sid_ev.lock().unwrap().clone();
                            let model = cur_model_ev.lock().unwrap().clone();
                            let mode = format!("{:?}", *cur_mode_ev.lock().unwrap()).to_lowercase();
                            move || {
                                if let Some(win) = w.upgrade() {
                                    win.set_queue_items(ModelRc::new(VecModel::from(items)));
                                    win.set_streaming_text(SharedString::new());
                                    win.set_streaming_messages(ModelRc::new(
                                        VecModel::<ChatMessage>::default(),
                                    ));
                                    win.set_thinking_text(SharedString::new());
                                    if queue_empty {
                                        win.set_agent_busy(false);
                                    }
                                    win.set_queue_count(queue_len as i32);
                                    // Save active session to disk after turn completes
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

/// Convert PlainChatMessage slice to sven_model::Message for Resubmit.
/// Merges consecutive assistant display blocks into single assistant messages.
/// Uses synthetic tool_call_ids for tool-call/tool-result pairs.
fn plain_messages_to_sven_messages(plain: &[PlainChatMessage]) -> Vec<SvenMessage> {
    use sven_frontend::segment::ChatSegment;
    let mut segments: Vec<ChatSegment> = Vec::new();
    let mut assistant_buf = String::new();
    let mut last_tool_call_id: Option<String> = None;
    let mut tool_call_counter = 0u32;

    for p in plain {
        match p.message_type {
            "user" => {
                if !assistant_buf.is_empty() {
                    segments.push(ChatSegment::Message(SvenMessage::assistant(
                        std::mem::take(&mut assistant_buf),
                    )));
                }
                segments.push(ChatSegment::Message(SvenMessage::user(&p.content)));
            }
            "assistant" | "code-block" | "heading" | "list-item" | "block-quote" | "separator"
            | "inline-code" | "table-row" => {
                if !assistant_buf.is_empty() {
                    assistant_buf.push('\n');
                }
                assistant_buf.push_str(&p.content);
            }
            "tool-call" => {
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
                if let Some(id) = last_tool_call_id.take() {
                    segments.push(ChatSegment::Message(SvenMessage::tool_result(
                        id, &p.content,
                    )));
                }
            }
            "system" => {
                if !assistant_buf.is_empty() {
                    segments.push(ChatSegment::Message(SvenMessage::assistant(
                        std::mem::take(&mut assistant_buf),
                    )));
                }
                segments.push(ChatSegment::Message(SvenMessage::system(&p.content)));
            }
            _ => {}
        }
    }
    if !assistant_buf.is_empty() {
        segments.push(ChatSegment::Message(SvenMessage::assistant(assistant_buf)));
    }
    sven_frontend::messages_for_resubmit(&segments)
}

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

/// Format todo items for display in the todo tool-call bubble.
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

/// Drain pending messages into the session store and optionally the window.
/// Only pushes to the window when `session_id` matches the active session.
/// Moves the session to the top of the sidebar when it receives new messages.
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
    // Move this session to the top of the sidebar (most recently active first)
    move_session_to_top(&win, session_id);
}

/// Update the last todo tool-call message with the given formatted todo list.
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

/// Move a session to the top of the sidebar list by id.
fn move_session_to_top(win: &MainWindow, session_id: &str) {
    let sessions_rc = win.get_sessions();
    if let Some(vec_model) = sessions_rc.as_any().downcast_ref::<VecModel<SessionItem>>() {
        for i in 0..vec_model.row_count() {
            if let Some(row) = vec_model.row_data(i) {
                if row.id == session_id && i > 0 {
                    let item = vec_model.remove(i);
                    vec_model.insert(0, item);
                    break;
                }
            }
        }
    }
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
