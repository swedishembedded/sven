use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::Context;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent, TaskTool};
use sven_input::{parse_conversation, serialize_conversation_turn};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::{
    events::{TodoItem, ToolEvent},
    AskQuestionTool, ApplyPatchTool, DeleteFileTool, EditFileTool,
    GlobFileSearchTool, GrepTool, ListDirTool, ReadFileTool, ReadLintsTool,
    RunTerminalCommandTool, SearchCodebaseTool, SwitchModeTool, TodoWriteTool,
    UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
    ToolRegistry,
};

use crate::output::{finalise_stdout, write_stderr, write_stdout};

/// Options for the conversation runner.
#[derive(Debug)]
pub struct ConversationOptions {
    pub mode: AgentMode,
    pub model_override: Option<String>,
    /// Path to the conversation markdown file to load and append to.
    pub file_path: PathBuf,
    /// The full file content (already read by the caller).
    pub content: String,
}

/// Headless runner for conversation files.
///
/// Loads conversation history from a markdown file, executes the trailing
/// `## User` section (if any), and appends the new turn back to the file.
pub struct ConversationRunner {
    config: Arc<Config>,
}

impl ConversationRunner {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub async fn run(&self, opts: ConversationOptions) -> anyhow::Result<()> {
        // Parse the conversation file
        let parsed = parse_conversation(&opts.content)
            .context("failed to parse conversation file")?;

        let pending = match parsed.pending_user_input {
            Some(p) => p,
            None => {
                write_stderr("[conversation] no pending ## User section found — nothing to execute");
                return Ok(());
            }
        };

        debug!(
            history_messages = parsed.history.len(),
            pending_len = pending.len(),
            "starting conversation turn"
        );

        // Build model
        let mut model_cfg = self.config.model.clone();
        if let Some(name) = &opts.model_override {
            const PROVIDER_KEYWORDS: &[&str] = &["mock", "openai", "anthropic"];
            if let Some((provider, model)) = name.split_once('/') {
                model_cfg.provider = provider.to_string();
                model_cfg.name = model.to_string();
            } else if PROVIDER_KEYWORDS.contains(&name.as_str()) {
                model_cfg.provider = name.clone();
            } else {
                model_cfg.name = name.clone();
            }
        }

        let model = sven_model::from_config(&model_cfg)
            .context("failed to initialise model provider")?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model);

        let agent_cfg = Arc::new(self.config.agent.clone());

        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
        let current_mode: Arc<Mutex<AgentMode>> = Arc::new(Mutex::new(opts.mode));
        let (tool_event_tx, _tool_event_rx) = mpsc::channel::<ToolEvent>(64);
        let task_depth = Arc::new(AtomicUsize::new(0));

        let tools = Arc::new(build_registry(
            &self.config,
            todos,
            current_mode,
            tool_event_tx,
            model.clone(),
            agent_cfg.clone(),
            task_depth,
        ));

        let mut agent = Agent::new(model, tools, agent_cfg, opts.mode, 128_000);

        // Load conversation history into the agent session.
        // replace_history_and_submit prepends the system message and then adds
        // the new user message, so we pass history (without pending) and the
        // pending string separately.
        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
        let submit_fut = agent.replace_history_and_submit(parsed.history, &pending, tx);

        // Collect events from agent into new messages for the file
        let mut new_messages: Vec<Message> = Vec::new();
        let mut failed = false;

        // Append the pending user message first (it was not in history)
        new_messages.push(Message::user(&pending));

        tokio::pin!(submit_fut);

        loop {
            tokio::select! {
                biased;

                Some(event) = rx.recv() => {
                    collect_event(event, &mut new_messages, &mut failed);
                }

                result = &mut submit_fut => {
                    if let Err(e) = result {
                        write_stderr(&format!("[fatal] {e}"));
                        std::process::exit(1);
                    }
                    while let Ok(ev) = rx.try_recv() {
                        collect_event(ev, &mut new_messages, &mut failed);
                    }
                    break;
                }
            }
        }

        // Finalise stdout (ensure trailing newline)
        let response_text: String = new_messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .filter_map(|m| m.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        finalise_stdout(&response_text);

        if failed {
            std::process::exit(1);
        }

        // Append new turn to the file (skip the User message — it's already there)
        let to_append = &new_messages[1..]; // skip the user message we prepended
        if !to_append.is_empty() {
            let md = serialize_conversation_turn(to_append);
            let mut file = OpenOptions::new()
                .append(true)
                .open(&opts.file_path)
                .with_context(|| format!("opening conversation file for append: {}", opts.file_path.display()))?;
            file.write_all(md.as_bytes())
                .with_context(|| "writing to conversation file")?;
            debug!(chars = md.len(), "appended to conversation file");
        }

        Ok(())
    }
}

// ── Event → Message collector ─────────────────────────────────────────────────

/// Translate an `AgentEvent` into messages/stdout, accumulating new `Message`
/// structs that will be serialised back into the conversation file.
fn collect_event(event: AgentEvent, messages: &mut Vec<Message>, failed: &mut bool) {
    match event {
        AgentEvent::TextDelta(delta) => {
            write_stdout(&delta);
        }

        AgentEvent::TextComplete(text) => {
            // Merge consecutive assistant text messages (streaming produces one
            // TextComplete per turn; tool calls produce interleaved ones).
            // Push as a new assistant message only if non-empty.
            if !text.is_empty() {
                messages.push(Message::assistant(&text));
            }
        }

        AgentEvent::ToolCallStarted(tc) => {
            write_stderr(&format!(
                "[tool] {} ({})",
                tc.name,
                serde_json::to_string(&tc.args).unwrap_or_default()
            ));
            // Record the tool call message so it appears in the file
            messages.push(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tc.id,
                    function: FunctionCall {
                        name: tc.name,
                        arguments: tc.args.to_string(),
                    },
                },
            });
        }

        AgentEvent::ToolCallFinished { call_id, tool_name, output, is_error } => {
            if is_error {
                write_stderr(&format!("[tool error] {tool_name}: {output}"));
            } else {
                write_stderr(&format!("[tool ok] {tool_name}"));
            }
            // Record the tool result
            messages.push(Message::tool_result(&call_id, &output));
        }

        AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
            write_stderr(&format!(
                "[compacted context: {tokens_before} → {tokens_after} tokens]"
            ));
        }

        AgentEvent::Error(msg) => {
            write_stderr(&format!("[agent error] {msg}"));
            *failed = true;
        }

        AgentEvent::TodoUpdate(todos) => {
            let lines: Vec<String> = todos.iter().map(|t| {
                let icon = match t.status.as_str() {
                    "completed" => "✓",
                    "in_progress" => "→",
                    "cancelled" => "✗",
                    _ => "○",
                };
                format!("  {icon} [{}] {}", t.id, t.content)
            }).collect();
            write_stderr(&format!("[todos]\n{}", lines.join("\n")));
        }

        AgentEvent::ModeChanged(mode) => {
            write_stderr(&format!("[mode changed] now in {mode} mode"));
        }

        AgentEvent::Question { questions, .. } => {
            write_stderr(&format!("[questions] {}", questions.join(" | ")));
        }

        AgentEvent::TurnComplete
        | AgentEvent::TokenUsage { .. }
        | AgentEvent::QuestionAnswer { .. } => {}
    }
}

// ── Registry builder (mirrors CiRunner) ──────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn build_registry(
    cfg: &Config,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    current_mode: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    model: Arc<dyn sven_model::ModelProvider>,
    agent_cfg: Arc<sven_config::AgentConfig>,
    task_depth: Arc<AtomicUsize>,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    reg.register(ReadFileTool);
    reg.register(ListDirTool);
    reg.register(GlobFileSearchTool);
    reg.register(GrepTool);
    reg.register(SearchCodebaseTool);
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });
    reg.register(ReadLintsTool);
    reg.register(UpdateMemoryTool {
        memory_file: cfg.tools.memory.memory_file.clone(),
    });
    reg.register(AskQuestionTool::new());
    reg.register(TodoWriteTool::new(todos, tool_event_tx.clone()));
    reg.register(SwitchModeTool::new(current_mode, tool_event_tx));
    reg.register(WriteTool);
    reg.register(EditFileTool);
    reg.register(DeleteFileTool);
    reg.register(ApplyPatchTool);
    reg.register(RunTerminalCommandTool {
        timeout_secs: cfg.tools.timeout_secs,
    });
    reg.register(TaskTool::new(model, Arc::new(cfg.clone()), agent_cfg, task_depth));

    reg
}
