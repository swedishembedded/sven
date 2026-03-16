// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Bridge between sven-frontend async agent events and the Slint UI model.

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use sven_config::{AgentMode, ModelConfig};
use sven_core::AgentEvent;
use sven_frontend::{
    agent_task,
    commands::{CommandContext, CommandRegistry, ParsedCommand},
    markdown::{parse_markdown_blocks, MarkdownBlock},
    node_agent_task,
    queue::QueueState,
    tool_view::extract_tool_view,
    AgentRequest, NodeBackend, QueuedMessage,
};
use sven_model::catalog;
use sven_tools::{OutputBufferStore, QuestionRequest, SharedToolDisplays};
use tokio::sync::{mpsc, Mutex as TokioMutex};

use crate::{ChatMessage, CompletionEntry, MainWindow, PickerItem, SessionItem, ToastItem};

// ── Plain-data structs for cross-thread communication ─────────────────────────

#[derive(Clone, Default)]
struct PlainChatMessage {
    message_type: &'static str,
    content: String,
    role: &'static str,

    // Group header
    is_first_in_group: bool,

    // State
    is_error: bool,
    is_streaming: bool,
    is_expanded: bool,

    // Tool call
    tool_name: String,
    tool_icon: String,
    tool_summary: String,
    tool_category: String,
    tool_fields_json: String,

    // Code block / heading
    language: String,
    heading_level: i32,
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
        }
    }
}

#[derive(Clone)]
struct PlainToast {
    message: String,
    level: &'static str,
}

/// Converts markdown text into a sequence of PlainChatMessages (one per block).
fn markdown_to_plain_messages(text: &str, role: &'static str) -> Vec<PlainChatMessage> {
    let blocks = parse_markdown_blocks(text);
    if blocks.is_empty() {
        // Fallback: show as a plain paragraph
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
            MarkdownBlock::CodeBlock { language, code } => PlainChatMessage {
                message_type: "code-block",
                content: code,
                role,
                is_first_in_group: is_first,
                language,
                ..Default::default()
            },
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

        let msgs_model = Rc::new(VecModel::<ChatMessage>::default());
        window.set_messages(ModelRc::from(msgs_model.clone()));
        window.set_toasts(ModelRc::new(VecModel::<ToastItem>::default()));
        window.set_model_name(SharedString::from(&opts.model_cfg.name));
        window.set_mode(SharedString::from(
            format!("{:?}", opts.mode).to_lowercase(),
        ));

        // Track current model/mode for the session
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

        // Message queue (for queuing while agent is busy)
        let queue_state: Arc<Mutex<QueueState>> = Arc::new(Mutex::new(QueueState::new()));

        // Title generation: send GenerateTitle on first user message
        let is_first_message: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));

        let weak = window.as_weak();

        // ── Toggle thinking/tool expand ───────────────────────────────────────
        let msgs_model_clone = Rc::clone(&msgs_model);
        window.on_toggle_expanded(move |idx| {
            let idx = idx as usize;
            if let Some(mut row) = msgs_model_clone.row_data(idx) {
                row.is_expanded = !row.is_expanded;
                msgs_model_clone.set_row_data(idx, row);
            }
        });

        // ── Send message callback ─────────────────────────────────────────────
        {
            let tx = agent_tx.clone();
            let pm_send = Arc::clone(&pending_msgs);
            let weak_send = weak.clone();
            let is_first = Arc::clone(&is_first_message);
            let queue = Arc::clone(&queue_state);
            let tx2 = agent_tx.clone();
            let cur_mode = Arc::clone(&current_mode);
            let cur_provider = Arc::clone(&current_model_provider);
            let cur_model = Arc::clone(&current_model_name);
            let config = Arc::clone(&opts.config);

            window.on_send_message(move |text| {
                let content = text.to_string();
                if content.is_empty() {
                    return;
                }

                // Immediately add the user message to the chat
                pm_send
                    .lock()
                    .unwrap()
                    .push_back(PlainChatMessage::user(&content));
                let pm2 = Arc::clone(&pm_send);
                let w = weak_send.clone();
                let _ = slint::invoke_from_event_loop(move || flush_messages(pm2, &w));

                // Check if agent is busy — if so, queue the message
                let agent_busy = weak_send
                    .upgrade()
                    .map(|w| w.get_agent_busy())
                    .unwrap_or(false);

                if agent_busy {
                    // Check if this is a slash command (process locally)
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
                            // Apply model/mode overrides to next queued message
                            if result.model_override.is_some() || result.mode_override.is_some() {
                                // Update session state
                                let mut q = queue.lock().unwrap();
                                q.push(QueuedMessage {
                                    content: result.message_to_send.unwrap_or_default(),
                                    model_transition: None,
                                    mode_transition: result.mode_override,
                                });
                                return;
                            }
                        }
                    }
                    queue.lock().unwrap().push(QueuedMessage {
                        content: content.clone(),
                        model_transition: None,
                        mode_transition: None,
                    });
                    return;
                }

                // Handle slash commands
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
                        if let Some(msg) = result.message_to_send {
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
                        return;
                    }
                }

                // Generate title on first real message
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
            let msgs_model_ns = Rc::clone(&msgs_model);
            let sessions_model_ns = Rc::clone(&sessions_model);
            let is_first_ns = Arc::clone(&is_first_message);
            let queue_ns = Arc::clone(&queue_state);
            let sb_ns = Arc::clone(&streaming_buf);
            let tb_ns = Arc::clone(&thinking_buf);
            let weak_ns = weak.clone();

            window.on_new_session(move || {
                // Reset message history
                while msgs_model_ns.row_count() > 0 {
                    msgs_model_ns.remove(0);
                }

                // Reset streaming buffers
                *sb_ns.lock().unwrap() = String::new();
                *tb_ns.lock().unwrap() = String::new();
                *queue_ns.lock().unwrap() = QueueState::new();
                *is_first_ns.lock().unwrap() = true;

                // Add new session entry
                let count = sessions_model_ns.row_count();
                let new_id = format!("session-{count}");

                // Deactivate all existing sessions
                for i in 0..sessions_model_ns.row_count() {
                    if let Some(mut s) = sessions_model_ns.row_data(i) {
                        s.active = false;
                        sessions_model_ns.set_row_data(i, s);
                    }
                }

                sessions_model_ns.push(SessionItem {
                    id: SharedString::from(new_id.clone()),
                    title: SharedString::from("New chat"),
                    busy: false,
                    active: true,
                    depth: 0,
                    status: SharedString::from("active"),
                    current_tool: SharedString::new(),
                    total_cost_usd: 0.0,
                });

                if let Some(win) = weak_ns.upgrade() {
                    win.set_active_session_id(SharedString::from(new_id));
                    win.set_streaming_text(SharedString::new());
                    win.set_thinking_text(SharedString::new());
                    win.set_agent_busy(false);
                }
            });
        }

        window.on_session_selected(|_| {});
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
                    let model = Rc::new(VecModel::from(entries));
                    win.set_completion_items(ModelRc::from(model));
                }
            });
        }

        // ── Completion accepted ───────────────────────────────────────────────
        {
            let weak_ca = weak.clone();
            window.on_completion_accepted(move |val| {
                // Replace the input text with the completed value
                if let Some(win) = weak_ca.upgrade() {
                    let current = win.get_input_text().to_string();
                    // Replace the command-being-typed with the accepted completion
                    let new_text = if let Some(stripped) = current.strip_prefix('/') {
                        let val = val.to_string();
                        if val.starts_with('/') {
                            format!("{val} ")
                        } else {
                            // This is an argument completion
                            let parts: Vec<&str> = stripped.splitn(2, ' ').collect();
                            if parts.len() <= 1 {
                                format!("/{val} ")
                            } else {
                                format!("/{} {val} ", parts[0])
                            }
                        }
                    } else {
                        format!("{val} ")
                    };
                    win.set_input_text(SharedString::from(new_text));
                    // Clear completions
                    win.set_completion_items(ModelRc::new(VecModel::<CompletionEntry>::default()));
                }
            });
        }

        // ── Model clicked ─────────────────────────────────────────────────────
        {
            let weak_mc = weak.clone();
            let config_mc = Arc::clone(&opts.config);
            let cur_provider_mc = Arc::clone(&current_model_provider);
            let cur_model_mc = Arc::clone(&current_model_name);

            window.on_model_clicked(move || {
                let current = format!(
                    "{}/{}",
                    cur_provider_mc.lock().unwrap(),
                    cur_model_mc.lock().unwrap()
                );

                // Build picker items from catalog + config
                let mut items: Vec<PickerItem> = Vec::new();
                items.push(PickerItem {
                    id: SharedString::from(current.clone()),
                    label: SharedString::from(format!("{} (current)", current)),
                    description: SharedString::new(),
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

                // Also add named config providers
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

                if let Some(win) = weak_mc.upgrade() {
                    let model = Rc::new(VecModel::from(items));
                    win.set_picker_items(ModelRc::from(model));
                    win.set_picker_title(SharedString::from("Switch model"));
                    win.set_picker_visible(true);
                }
            });
        }

        // ── Mode clicked ──────────────────────────────────────────────────────
        {
            let weak_mc = weak.clone();
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
                if let Some(win) = weak_mc.upgrade() {
                    let model = Rc::new(VecModel::from(items));
                    win.set_picker_items(ModelRc::from(model));
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
                if let Some(win) = weak_ps.upgrade() {
                    win.set_picker_visible(false);
                    let title = win.get_picker_title().to_string();

                    if title.contains("mode") {
                        let mode = match id.as_str() {
                            "plan" => AgentMode::Plan,
                            "research" => AgentMode::Research,
                            _ => AgentMode::Agent,
                        };
                        *cur_mode_ps.lock().unwrap() = mode;
                        win.set_mode(SharedString::from(id.clone()));
                    } else {
                        // Model selection
                        let parts: Vec<&str> = id.splitn(2, '/').collect();
                        if parts.len() == 2 {
                            *cur_provider_ps.lock().unwrap() = parts[0].to_string();
                            *cur_model_ps.lock().unwrap() = parts[1].to_string();
                            win.set_model_name(SharedString::from(parts[1]));
                        } else {
                            *cur_model_ps.lock().unwrap() = id.clone();
                            win.set_model_name(SharedString::from(id.clone()));
                        }
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
                        let text = buf.clone();
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
                        *tb.lock().unwrap() = String::new();
                        // Thinking blocks are committed collapsed
                        pm.lock().unwrap().push_back(PlainChatMessage {
                            message_type: "thinking",
                            content,
                            role: "thinking",
                            is_first_in_group: false,
                            is_expanded: false, // collapsed after streaming completes
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
                            is_expanded: false, // collapsed by default
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

                        // Dequeue next message if available
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
                                    // Update the active session title
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

                // Update sessions busy indicator
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

/// Format tool fields as a readable multi-line string for the expanded view.
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

/// Drain pending chat messages into the window's messages model.
/// Must be called on the Slint main thread.
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
            while vec_model.row_count() > 5 {
                vec_model.remove(0);
            }
        }
    }
}
