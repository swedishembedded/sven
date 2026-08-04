// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Per-step event handler: folds `AgentEvent`s into a
//! [`StepAssembler`](sven_input::trace_session::StepAssembler), collects
//! plain `Message`s for the legacy markdown history/artifact paths, and
//! tracks token usage.

use sven_core::AgentEvent;
use sven_input::trace_session::StepAssembler;
use sven_model::{FunctionCall, Message, MessageContent, Role};

use crate::output::{format_token_usage_line, write_stderr, write_stdout};

use super::{OutputFormat, EXIT_BUDGET_EXHAUSTED};

/// Stream every step the assembler has newly closed (since `emitted`) to
/// stdout as NDJSON — one ATIF `TraceStep` JSON object per line — when
/// `output_format` is `Jsonl`. Always advances `*emitted` to the assembler's
/// current closed-step count, regardless of output format, so a later call
/// only ever sees steps closed after this one.
///
/// This is the single place that keeps real-time JSONL streaming consistent
/// with the turn-shaped step assembler: a step is only known complete (and
/// thus emittable) once something closes it — a user message, a fresh
/// thinking block, a context-compaction marker, or the final flush at the
/// end of the run. That is coarser-grained than the old one-record-per-raw-
/// event stream, but matches ATIF's turn-shaped `StepObject`: a `ToolCall`
/// and its result are one step, not two independent lines.
pub(super) fn stream_new_steps(assembler: &StepAssembler, emitted: &mut usize, output_format: OutputFormat) {
    let closed = assembler.closed_steps();
    if output_format == OutputFormat::Jsonl {
        for step in &closed[*emitted..] {
            match serde_json::to_string(step) {
                Ok(line) => write_stdout(&format!("{line}\n")),
                Err(e) => write_stderr(&format!("[sven:warn] Failed to serialize trace step: {e}")),
            }
        }
    }
    *emitted = closed.len();
}

/// Per-step mutable state threaded through the event handler.
pub(super) struct StepState<'a> {
    pub response_text: &'a mut String,
    pub tools_used: &'a mut Vec<String>,
    pub failed: &'a mut bool,
    pub collected: &'a mut Vec<Message>,
    /// Turn assembler accumulating this run's `TraceStep`s.
    pub assembler: &'a mut StepAssembler,
    /// Number of `assembler.closed_steps()` already streamed to stdout (only
    /// meaningful when `output_format == Jsonl`); see [`stream_new_steps`].
    pub emitted_steps: &'a mut usize,
    pub consecutive_tool_errors: &'a mut u32,
    pub trace_level: u8,
    pub output_format: OutputFormat,
    pub sven_header_emitted: &'a mut bool,
    /// Running total of non-cached input tokens across the whole session.
    pub session_input_total: &'a mut u32,
    /// Running total of output tokens across the whole session.
    pub session_output_total: &'a mut u32,
    /// Accumulates `true` when any tool call returns an error (non-fatal).
    pub any_tool_errors: &'a mut bool,
    /// Running total of tokens used across all steps (input + output).
    pub run_total_tokens: &'a mut u64,
    /// Optional token budget cap; when exceeded the runner exits with code 4.
    pub max_tokens_budget: Option<u64>,
}
/// Process a single agent event: write diagnostics to stderr, collect
/// messages into `collected`, fold the event into `assembler`, and track
/// response text / tool usage.
pub(super) fn handle_event(event: AgentEvent, s: &mut StepState<'_>) {
    let response_text = &mut *s.response_text;
    let tools_used = &mut *s.tools_used;
    let failed = &mut *s.failed;
    let collected = &mut *s.collected;
    let assembler = &mut *s.assembler;
    let consecutive_tool_errors = &mut *s.consecutive_tool_errors;
    let trace_level = s.trace_level;
    let output_format = s.output_format;
    let sven_header_emitted = &mut *s.sven_header_emitted;
    match event {
        AgentEvent::TextDelta(delta) => {
            response_text.push_str(&delta);
            // Stream to stdout in real-time for conversation format.
            if output_format == OutputFormat::Conversation {
                if !*sven_header_emitted {
                    write_stdout("## Sven\n");
                    *sven_header_emitted = true;
                }
                write_stdout(&delta);
            }
        }
        AgentEvent::TextComplete(text) => {
            if !text.is_empty() {
                let msg = Message::assistant(&text);
                collected.push(msg.clone());
                assembler.push_message(&msg);
                stream_new_steps(assembler, s.emitted_steps, output_format);
                // Ensure trailing newline after streamed text in conversation format
                if output_format == OutputFormat::Conversation && *sven_header_emitted {
                    if !text.ends_with('\n') {
                        write_stdout("\n\n");
                    } else {
                        write_stdout("\n");
                    }
                    *sven_header_emitted = false;
                }
            }
        }
        AgentEvent::ToolCallStarted(tc) => {
            write_stderr(&format!(
                "[sven:tool:call] id=\"{}\" name=\"{}\" args={}",
                tc.id,
                tc.name,
                serde_json::to_string(&tc.args).unwrap_or_default()
            ));
            tools_used.push(tc.name.clone());
            let args_str = serde_json::to_string(&tc.args).unwrap_or_default();
            let msg = Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tc.id.clone(),
                    function: FunctionCall {
                        name: tc.name.clone(),
                        arguments: args_str.clone(),
                    },
                },
            };
            // Stream tool call section to stdout in conversation format
            if output_format == OutputFormat::Conversation {
                // Ensure any open Sven text section is closed first
                if *sven_header_emitted {
                    write_stdout("\n\n");
                    *sven_header_emitted = false;
                }
                let args_value: serde_json::Value =
                    serde_json::from_str(&args_str).unwrap_or(serde_json::Value::Null);
                let envelope = serde_json::json!({
                    "tool_call_id": tc.id,
                    "name": tc.name,
                    "args": args_value,
                });
                let pretty = serde_json::to_string_pretty(&envelope).unwrap_or_default();
                write_stdout(&format!("## Tool\n```json\n{pretty}\n```\n\n"));
            }
            collected.push(msg.clone());
            assembler.push_message(&msg);
            stream_new_steps(assembler, s.emitted_steps, output_format);
        }
        AgentEvent::ToolCallFinished {
            call_id,
            tool_name,
            is_error,
            output,
        } => {
            if is_error {
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" success=false output={output:?}"
                ));
                *consecutive_tool_errors += 1;
                *s.any_tool_errors = true;
            } else {
                let output_snippet = if trace_level >= 1 && !output.is_empty() {
                    const LIMIT: usize = 1500;
                    let preview: String = output.chars().take(LIMIT).collect();
                    if output.chars().count() > LIMIT {
                        format!(
                            " output={:?}...[+{} chars]",
                            preview,
                            output.chars().count() - LIMIT
                        )
                    } else {
                        format!(" output={output:?}")
                    }
                } else {
                    String::new()
                };
                write_stderr(&format!(
                    "[sven:tool:result] id=\"{call_id}\" name=\"{tool_name}\" success=true size={}{}",
                    output.len(),
                    output_snippet
                ));
                *consecutive_tool_errors = 0;
            }
            // Stream tool result section to stdout in conversation format
            if output_format == OutputFormat::Conversation {
                write_stdout(&format!("## Tool Result\n```\n{output}\n```\n\n"));
            }
            let msg = Message::tool_result(&call_id, &output);
            collected.push(msg.clone());
            assembler.push_message(&msg);
            stream_new_steps(assembler, s.emitted_steps, output_format);
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
            let strategy_str = strategy.to_string();
            assembler.push_context_compacted(tokens_before, tokens_after, Some(&strategy_str), Some(turn));
            stream_new_steps(assembler, s.emitted_steps, output_format);
        }
        AgentEvent::Error(msg) => {
            write_stderr(&format!("[sven:agent:error] {msg}"));
            *failed = true;
        }
        AgentEvent::TodoUpdate(todos) => {
            let lines: Vec<String> = todos
                .iter()
                .map(|t| {
                    let icon = t.status.icon();
                    format!("  {icon} [{}] {}", t.id, t.content)
                })
                .collect();
            write_stderr(&format!("[sven:todos]\n{}", lines.join("\n")));
        }
        AgentEvent::ModeChanged(mode) => {
            write_stderr(&format!("[sven:mode:changed] now in {mode} mode"));
        }
        AgentEvent::ModelChanged(model) => {
            write_stderr(&format!("[sven:model:changed] switching to {model}"));
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
            max_output_tokens,
            cost_usd: _,
        } => {
            *s.session_input_total += input;
            *s.session_output_total += output;
            *s.run_total_tokens += (input + output) as u64;
            if let Some(budget) = s.max_tokens_budget {
                if budget > 0 && *s.run_total_tokens >= budget {
                    write_stderr(&format!(
                        "[sven:error] Token budget exhausted: {} tokens used (budget: {}). Stopping.",
                        s.run_total_tokens, budget
                    ));
                    std::process::exit(EXIT_BUDGET_EXHAUSTED);
                }
            }
            let mut line = format_token_usage_line(
                input,
                output,
                cache_read,
                cache_write,
                cache_read_total,
                cache_write_total,
                max_tokens,
                max_output_tokens,
            );
            line.push_str(&format!(
                " input_total={} output_total={}",
                s.session_input_total, s.session_output_total
            ));
            write_stderr(&line);
        }
        AgentEvent::ThinkingDelta(_) => {}
        AgentEvent::ThinkingComplete(content) => {
            write_stderr(&format!("[sven:thinking] {content}"));
            assembler.push_thinking(&content);
            stream_new_steps(assembler, s.emitted_steps, output_format);
        }
        AgentEvent::ToolProgress { message, .. } => {
            write_stderr(&format!("[sven:progress] {message}"));
        }
        AgentEvent::TurnComplete
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::CollabEvent(_)
        | AgentEvent::TitleGenerated(_)
        | AgentEvent::DelegateSummary { .. }
        | AgentEvent::SubagentStarted { .. }
        | AgentEvent::SubagentEvent { .. }
        | AgentEvent::PeerList(_) => {}
        AgentEvent::Aborted { partial_text } => {
            if !partial_text.is_empty() {
                write_stderr(&format!("[sven:agent:aborted] partial={:?}", partial_text));
                let msg = Message::assistant(&partial_text);
                collected.push(msg.clone());
                assembler.push_message(&msg);
                stream_new_steps(assembler, s.emitted_steps, output_format);
            } else {
                write_stderr("[sven:agent:aborted]");
            }
        }
    }
}
