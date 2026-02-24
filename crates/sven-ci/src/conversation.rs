// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::Context;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentMode, Config};
use sven_core::AgentEvent;
use sven_bootstrap::{AgentBuilder, ToolSetProfile};
use sven_input::{
    parse_conversation, parse_jsonl_conversation,
    serialize_conversation_turn_with_metadata, serialize_jsonl_conversation_turn,
    TurnMetadata,
};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::events::TodoItem;

use crate::output::{finalise_stdout, write_stderr, write_stdout};

/// Options for the conversation runner.
#[derive(Debug)]
pub struct ConversationOptions {
    pub mode: AgentMode,
    pub model_override: Option<String>,
    /// Path to the conversation file to load and append to.
    /// Supports both `.md` (markdown) and `.jsonl` (raw serde JSONL) formats.
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
        // Detect file format by extension; default to markdown.
        let is_jsonl = opts.file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jsonl"))
            .unwrap_or(false);

        // Parse the conversation file
        let parsed = if is_jsonl {
            parse_jsonl_conversation(&opts.content)
                .context("failed to parse JSONL conversation file")?
        } else {
            parse_conversation(&opts.content)
                .context("failed to parse conversation file")?
        };

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

        // Build model config, respecting config.providers for named overrides.
        let model_cfg = if let Some(name) = &opts.model_override {
            sven_model::resolve_model_from_config(&self.config, name)
        } else {
            self.config.model.clone()
        };

        let model = sven_model::from_config(&model_cfg)
            .context("failed to initialise model provider")?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model);

        // Build metadata for turn annotation
        let turn_metadata = TurnMetadata {
            provider: Some(model_cfg.provider.clone()),
            model: Some(model_cfg.name.clone()),
            timestamp: None,
        };

        // The mode lock and tool-event channel are created inside
        // AgentBuilder::build() so that SwitchModeTool and the agent loop
        // share the same instances.
        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
        let task_depth = Arc::new(AtomicUsize::new(0));

        let profile = ToolSetProfile::Full { question_tx: None, todos, task_depth };

        let mut agent = AgentBuilder::new(self.config.clone())
            .build(opts.mode, model.clone(), profile);

        // Load conversation history into the agent session.
        // replace_history_and_submit prepends the system message and then adds
        // the new user message, so we pass history (without pending) and the
        // pending string separately.
        //
        // The submit_fut holds a mutable borrow on `agent`. Scoping it in a
        // block ensures it is dropped before we need to call agent.session()
        // for the --jsonl-output path below.
        let (new_messages, failed) = {
            let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
            let submit_fut = agent.replace_history_and_submit(parsed.history, &pending, tx);

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
                            write_stderr(&format!("[fatal] {e:#}"));
                            std::process::exit(1);
                        }
                        while let Ok(ev) = rx.try_recv() {
                            collect_event(ev, &mut new_messages, &mut failed);
                        }
                        break;
                    }
                }
            }
            // submit_fut dropped here — mutable borrow on agent released
            (new_messages, failed)
        };

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
            let serialized = if is_jsonl {
                serialize_jsonl_conversation_turn(to_append)
            } else {
                serialize_conversation_turn_with_metadata(to_append, Some(&turn_metadata))
            };
            let mut file = OpenOptions::new()
                .append(true)
                .open(&opts.file_path)
                .with_context(|| format!("opening conversation file for append: {}", opts.file_path.display()))?;
            file.write_all(serialized.as_bytes())
                .with_context(|| "writing to conversation file")?;
            debug!(chars = serialized.len(), "appended to conversation file");
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
                "[sven:tool:call] id=\"{}\" name=\"{}\" args={}",
                tc.id,
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
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" success=false output={output:?}"
                ));
            } else {
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" success=true size={}",
                    output.len()
                ));
            }
            // Record the tool result
            messages.push(Message::tool_result(&call_id, &output));
        }

        AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
            write_stderr(&format!(
                "[sven:context:compacted] {tokens_before} → {tokens_after} tokens"
            ));
        }

        AgentEvent::Error(msg) => {
            write_stderr(&format!("[sven:agent:error] {msg}"));
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
            write_stderr(&format!("[sven:todos]\n{}", lines.join("\n")));
        }

        AgentEvent::ModeChanged(mode) => {
            write_stderr(&format!("[sven:mode:changed] now in {mode} mode"));
        }

        AgentEvent::Question { questions, .. } => {
            write_stderr(&format!("[sven:questions] {}", questions.join(" | ")));
        }

        AgentEvent::TokenUsage { input, output, cache_read, cache_write, .. } => {
            if cache_read > 0 || cache_write > 0 {
                write_stderr(&format!(
                    "[sven:tokens] input={input} output={output} cache_read={cache_read} cache_write={cache_write}"
                ));
            } else {
                write_stderr(&format!("[sven:tokens] input={input} output={output}"));
            }
        }

        AgentEvent::ThinkingDelta(_) => {}

        AgentEvent::ThinkingComplete(content) => {
            write_stderr(&format!("[sven:thinking] {content}"));
        }

        AgentEvent::TurnComplete | AgentEvent::QuestionAnswer { .. } => {}
    }
}
