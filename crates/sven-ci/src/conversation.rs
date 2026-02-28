// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_bootstrap::{AgentBuilder, ToolSetProfile};
use sven_config::{AgentMode, Config};
use sven_core::AgentEvent;
use sven_input::{
    parse_conversation, parse_jsonl_full, serialize_conversation_turn_with_metadata,
    serialize_jsonl_records, ConversationRecord, TurnMetadata,
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
        let is_jsonl = opts
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("jsonl"))
            .unwrap_or(false);

        // Parse the conversation file.
        // For JSONL files: use the full-fidelity parser which handles both
        // the new ConversationRecord format and the legacy raw-Message format.
        let (history, pending, existing_jsonl_records) = if is_jsonl {
            let parsed = parse_jsonl_full(&opts.content)
                .context("failed to parse JSONL conversation file")?;
            let pending = match parsed.pending_user_input {
                Some(p) => p,
                None => {
                    write_stderr(
                        "[conversation] no pending ## User section found — nothing to execute",
                    );
                    return Ok(());
                }
            };
            (parsed.history, pending, Some(parsed.records))
        } else {
            let parsed =
                parse_conversation(&opts.content).context("failed to parse conversation file")?;
            let pending = match parsed.pending_user_input {
                Some(p) => p,
                None => {
                    write_stderr(
                        "[conversation] no pending ## User section found — nothing to execute",
                    );
                    return Ok(());
                }
            };
            (parsed.history, pending, None)
        };

        debug!(
            history_messages = history.len(),
            pending_len = pending.len(),
            "starting conversation turn"
        );

        // Build model config, respecting config.providers for named overrides.
        let model_cfg = if let Some(name) = &opts.model_override {
            sven_model::resolve_model_from_config(&self.config, name)
        } else {
            self.config.model.clone()
        };

        let model =
            sven_model::from_config(&model_cfg).context("failed to initialise model provider")?;
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

        let profile = ToolSetProfile::Full {
            question_tx: None,
            todos,
            task_depth,
        };

        let mut agent = AgentBuilder::new(self.config.clone())
            .build(opts.mode, model.clone(), profile)
            .await;

        // Load conversation history into the agent session.
        // replace_history_and_submit prepends the system message and then adds
        // the new user message, so we pass history (without pending) and the
        // pending string separately.
        //
        // The submit_fut holds a mutable borrow on `agent`. Scoping it in a
        // block ensures it is dropped before we need to call agent.session()
        // for subsequent operations below.
        let (new_records, failed) = {
            let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
            let submit_fut = agent.replace_history_and_submit(history, &pending, tx);

            // Collect full-fidelity records including thinking blocks.
            // The pending user message is first (not yet in the file for md format;
            // already in the file for JSONL format — handled at write time below).
            let mut new_records: Vec<ConversationRecord> = Vec::new();
            new_records.push(ConversationRecord::Message(Message::user(&pending)));
            let mut failed = false;

            tokio::pin!(submit_fut);

            loop {
                tokio::select! {
                    biased;

                    Some(event) = rx.recv() => {
                        collect_event_full(event, &mut new_records, &mut failed);
                    }

                    result = &mut submit_fut => {
                        if let Err(e) = result {
                            write_stderr(&format!("[fatal] {e:#}"));
                            std::process::exit(1);
                        }
                        while let Ok(ev) = rx.try_recv() {
                            collect_event_full(ev, &mut new_records, &mut failed);
                        }
                        break;
                    }
                }
            }
            // submit_fut dropped here — mutable borrow on agent released
            (new_records, failed)
        };

        // Finalise stdout (ensure trailing newline)
        let response_text: String = new_records
            .iter()
            .filter_map(|r| match r {
                ConversationRecord::Message(m) if m.role == Role::Assistant => m.as_text(),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        finalise_stdout(&response_text);

        if failed {
            std::process::exit(1);
        }

        // Write the updated conversation back to the file.
        if is_jsonl {
            // For JSONL: rewrite the entire file so that thinking blocks and
            // new-format records are included.  Start from the existing records
            // already in the file, then append the new turn (skip the user
            // record — it is already present as the last line of the file).
            let mut all_records = existing_jsonl_records.unwrap_or_default();
            // Skip the first new_record (the user message that was already in the file)
            all_records.extend_from_slice(&new_records[1..]);
            let serialized = serialize_jsonl_records(&all_records);
            std::fs::write(&opts.file_path, serialized.as_bytes()).with_context(|| {
                format!(
                    "writing JSONL conversation file: {}",
                    opts.file_path.display()
                )
            })?;
            debug!(
                records = all_records.len(),
                "rewrote JSONL conversation file"
            );
        } else {
            // For markdown: append new messages only (excluding the user, which
            // is already present), using the legacy serializer.
            let new_messages: Vec<Message> = new_records[1..]
                .iter()
                .filter_map(|r| {
                    if let ConversationRecord::Message(m) = r {
                        Some(m.clone())
                    } else {
                        None
                    }
                })
                .collect();
            if !new_messages.is_empty() {
                let serialized =
                    serialize_conversation_turn_with_metadata(&new_messages, Some(&turn_metadata));
                let mut file = OpenOptions::new()
                    .append(true)
                    .open(&opts.file_path)
                    .with_context(|| {
                        format!(
                            "opening conversation file for append: {}",
                            opts.file_path.display()
                        )
                    })?;
                file.write_all(serialized.as_bytes())
                    .with_context(|| "writing to conversation file")?;
                debug!(chars = serialized.len(), "appended to conversation file");
            }
        }

        Ok(())
    }
}

// ── Full-fidelity event → ConversationRecord collector ────────────────────────

/// Translate an `AgentEvent` into `ConversationRecord`s, capturing every
/// element of the conversation including thinking/reasoning blocks, tool calls,
/// tool results (with image parts), and assistant text.
///
/// This replaces the old `collect_event` which only captured `Message`s and
/// silently discarded thinking traces.
fn collect_event_full(event: AgentEvent, records: &mut Vec<ConversationRecord>, failed: &mut bool) {
    match event {
        AgentEvent::TextDelta(delta) => {
            write_stdout(&delta);
        }

        AgentEvent::TextComplete(text) => {
            if !text.is_empty() {
                records.push(ConversationRecord::Message(Message::assistant(&text)));
            }
        }

        AgentEvent::ThinkingDelta(_) => {}

        AgentEvent::ThinkingComplete(content) => {
            write_stderr(&format!("[sven:thinking] {content}"));
            records.push(ConversationRecord::Thinking { content });
        }

        AgentEvent::ToolCallStarted(tc) => {
            write_stderr(&format!(
                "[sven:tool:call] id=\"{}\" name=\"{}\" args={}",
                tc.id,
                tc.name,
                serde_json::to_string(&tc.args).unwrap_or_default()
            ));
            records.push(ConversationRecord::Message(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tc.id,
                    function: FunctionCall {
                        name: tc.name,
                        arguments: tc.args.to_string(),
                    },
                },
            }));
        }

        AgentEvent::ToolCallFinished {
            call_id,
            tool_name,
            output,
            is_error,
        } => {
            if is_error {
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" \
                     success=false output={output:?}"
                ));
            } else {
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" \
                     success=true size={}",
                    output.len()
                ));
            }
            records.push(ConversationRecord::Message(Message::tool_result(
                &call_id, &output,
            )));
        }

        AgentEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => {
            let turn_note = if turn > 0 {
                format!(" (tool round {turn})")
            } else {
                String::new()
            };
            write_stderr(&format!(
                "[sven:context:compacted:{strategy}] {tokens_before} → {tokens_after} tokens{turn_note}"
            ));
            records.push(ConversationRecord::ContextCompacted {
                tokens_before,
                tokens_after,
                strategy: Some(strategy.to_string()),
                turn: Some(turn),
            });
        }

        AgentEvent::Error(msg) => {
            write_stderr(&format!("[sven:agent:error] {msg}"));
            *failed = true;
        }

        AgentEvent::TodoUpdate(todos) => {
            let lines: Vec<String> = todos
                .iter()
                .map(|t| {
                    let icon = match t.status.as_str() {
                        "completed" => "✓",
                        "in_progress" => "→",
                        "cancelled" => "✗",
                        _ => "○",
                    };
                    format!("  {icon} [{}] {}", t.id, t.content)
                })
                .collect();
            write_stderr(&format!("[sven:todos]\n{}", lines.join("\n")));
        }

        AgentEvent::ModeChanged(mode) => {
            write_stderr(&format!("[sven:mode:changed] now in {mode} mode"));
        }

        AgentEvent::Question { questions, .. } => {
            write_stderr(&format!("[sven:questions] {}", questions.join(" | ")));
        }

        AgentEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
        } => {
            let total_ctx = input + cache_read + cache_write;
            let ctx_pct = if max_tokens > 0 {
                ((total_ctx as u64 * 100) / max_tokens as u64).min(100) as u32
            } else {
                0
            };
            let ctx_cache = if total_ctx > 0 {
                cache_read * 100 / total_ctx
            } else {
                0
            };
            let mut line = format!("[sven:tokens] input={input} output={output}");
            if cache_read > 0 || cache_write > 0 {
                line.push_str(&format!(
                    " cache_read={cache_read} cache_write={cache_write}"
                ));
            }
            if max_tokens > 0 {
                line.push_str(&format!(" ctx_pct={ctx_pct} ctx_cache={ctx_cache}"));
            }
            if cache_read_total > 0 || cache_write_total > 0 {
                line.push_str(&format!(
                    " cache_read_total={cache_read_total} cache_write_total={cache_write_total}"
                ));
            }
            write_stderr(&line);
        }

        AgentEvent::TurnComplete | AgentEvent::QuestionAnswer { .. } => {}
        AgentEvent::Aborted { partial_text } => {
            if !partial_text.is_empty() {
                write_stderr(&format!("[sven:agent:aborted] partial={:?}", partial_text));
                records.push(ConversationRecord::Message(Message::assistant(
                    &partial_text,
                )));
            } else {
                write_stderr("[sven:agent:aborted]");
            }
        }
    }
}
