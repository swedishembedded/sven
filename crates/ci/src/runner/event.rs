// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Per-step event handler: folds `AgentEvent`s into a
//! [`StepAssembler`](sven_session_store::trace_session::StepAssembler), collects
//! plain `Message`s for the legacy markdown history/artifact paths, and
//! tracks token usage.

use std::collections::HashMap;

use atif::Trajectory;
use sven_machines::AgentEvent;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_session_store::trace_session::{self, StepAssembler};
use sven_session_store::OutcomeFold;
use sven_tools::events::SubagentUpdate;

use crate::output::{format_token_usage_line, tool_output_snippet, write_stderr, write_stdout};

use super::OutputFormat;

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
pub(super) fn stream_new_steps(
    assembler: &StepAssembler,
    emitted: &mut usize,
    output_format: OutputFormat,
) {
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

/// In-progress state for a subagent spawned via the `task` tool, keyed by
/// its ACP buffer `handle_id` (see `AgentEvent::SubagentStarted` /
/// `AgentEvent::SubagentEvent`). Accumulates the subagent's own turn stream
/// into an independent [`StepAssembler`], scoped to just this child, until a
/// [`SubagentUpdate::Finished`]/[`SubagentUpdate::Failed`] closes it out —
/// at which point [`finalize_subagent_child`] folds it into a standalone
/// [`Trajectory`] and embeds it into the parent's own trace (see
/// [`handle_event`]'s `AgentEvent::SubagentEvent` arm).
pub(super) struct SubagentChildState {
    /// Document identity minted for this child at spawn time (with
    /// [`trace_session::new_session_id`] — the same helper the rest of this
    /// codebase mints session/trajectory ids with; there is no separate
    /// trajectory-id-only minting helper).
    trajectory_id: String,
    /// Accumulates the child's own `TraceStep`s as its ACP updates arrive.
    assembler: StepAssembler,
    /// Buffered `TextDelta`s not yet folded into `assembler`. Unlike the
    /// parent run's own `AgentEvent::TextDelta`/`TextComplete` pair,
    /// [`SubagentUpdate`] has no "text complete" signal — deltas are
    /// buffered here and flushed into `assembler` the next time a turn
    /// boundary is known (a tool call starting, or the subagent finishing).
    text_buf: String,
    /// Buffered `ThinkingDelta`s, flushed the same way.
    thinking_buf: String,
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
    /// Optional token budget cap; when exceeded, `handle_event` sets
    /// `*budget_exhausted = true` rather than exiting the process directly
    /// (see that field's doc comment for why).
    pub max_tokens_budget: Option<u64>,
    /// Set to `true` by the `TokenUsage` handler when `max_tokens_budget` is
    /// exceeded. `handle_event` deliberately does **not** call
    /// `std::process::exit` itself for this condition: unlike the CI
    /// runner's other fatal-abort sites (timeout, consecutive tool errors,
    /// step failure — all in `runner/mod.rs`, where `flush_trace` and
    /// `effective_output_trace` are in scope), this handler has no access to
    /// the trace-flush closure, so exiting here would skip it entirely and
    /// the run would end with `--output-trace` unwritten. Instead, the
    /// caller (`runner/mod.rs`) checks this flag immediately after each
    /// `handle_event` call — mirroring the existing
    /// `consecutive_tool_errors >= MAX_CONSECUTIVE_TOOL_ERRORS` check right
    /// next to it — and flushes the partial trace before exiting with
    /// `EXIT_BUDGET_EXHAUSTED` itself.
    pub budget_exhausted: &'a mut bool,
    /// In-progress subagent child state, keyed by ACP `handle_id`; see
    /// [`SubagentChildState`].
    pub subagent_children: &'a mut HashMap<String, SubagentChildState>,
    /// Subagent trajectories that finished (or failed) during this event and
    /// are ready to embed into the parent document. `runner/mod.rs`'s
    /// `flush_trace` appends these into `Trajectory.subagent_trajectories`
    /// on every flush, so a mid-run crash still yields a valid partial
    /// document with whatever subagents completed so far.
    pub completed_subagents: &'a mut Vec<Trajectory>,
    /// Outcome tally for this run's reward stamp. Reward-only: it deliberately
    /// does **not** feed the exit-code verdict (`failed`, `any_tool_errors`,
    /// `consecutive_tool_errors` still own that), so scoring can be retuned
    /// without changing what CI reports.
    pub outcome: &'a mut OutcomeFold,
}
/// Process a single agent event: write diagnostics to stderr, collect
/// messages into `collected`, fold the event into `assembler`, and track
/// response text / tool usage.
pub(super) fn handle_event(event: AgentEvent, s: &mut StepState<'_>) {
    // Fold every event into the reward tally first — one insertion point that
    // no future arm can forget.
    s.outcome.observe(&event);
    let response_text = &mut *s.response_text;
    let tools_used = &mut *s.tools_used;
    let failed = &mut *s.failed;
    let collected = &mut *s.collected;
    let assembler = &mut *s.assembler;
    let consecutive_tool_errors = &mut *s.consecutive_tool_errors;
    let trace_level = s.trace_level;
    let output_format = s.output_format;
    let sven_header_emitted = &mut *s.sven_header_emitted;
    let subagent_children = &mut *s.subagent_children;
    let completed_subagents = &mut *s.completed_subagents;
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
                let output_snippet = tool_output_snippet(trace_level, &output);
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
            assembler.push_context_compacted(
                tokens_before,
                tokens_after,
                Some(&strategy_str),
                Some(turn),
            );
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
                    // Signal the caller rather than exiting here — see
                    // `StepState::budget_exhausted`'s doc comment.
                    *s.budget_exhausted = true;
                }
            }
            // Usage often arrives before TextComplete (many providers send
            // the usage chunk as the last SSE frame ahead of [DONE]). Close
            // any open `## Sven` streaming section first so this stderr
            // write doesn't land glued onto the end of the still-open
            // stdout text with no newline between them.
            if output_format == OutputFormat::Conversation && *sven_header_emitted {
                write_stdout("\n\n");
                *sven_header_emitted = false;
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
        | AgentEvent::Transition { .. }
        | AgentEvent::PeerList(_) => {}
        // A delegate subtree completed (a distinct feature from the `task`
        // tool's ACP subagents above). It carries only a condensed summary,
        // not a structured
        // conversation stream, so there is nothing to fold into an embedded
        // `Trajectory`; still emit a stderr trace token so the delegation is
        // at least visible (previously silently dropped).
        AgentEvent::DelegateSummary {
            to_name,
            task_title,
            duration_ms,
            status,
            result_preview,
        } => {
            write_stderr(&format!(
                "[sven:subagent:delegate_summary] to=\"{to_name}\" task={task_title:?} status=\"{status}\" duration_ms={duration_ms} result_preview={result_preview:?}"
            ));
        }
        AgentEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            prompt,
        } => {
            write_stderr(&format!(
                "[sven:subagent:started] call_id=\"{call_id}\" handle_id=\"{handle_id}\" description={description:?}"
            ));
            let trajectory_id = trace_session::new_session_id();
            let mut child_assembler = StepAssembler::new();
            if !prompt.is_empty() {
                child_assembler.push_message(&Message::user(&prompt));
            }
            subagent_children.insert(
                handle_id,
                SubagentChildState {
                    trajectory_id,
                    assembler: child_assembler,
                    text_buf: String::new(),
                    thinking_buf: String::new(),
                },
            );
        }
        AgentEvent::SubagentEvent {
            call_id,
            handle_id,
            update,
        } => {
            handle_subagent_update(
                &call_id,
                handle_id,
                update,
                subagent_children,
                assembler,
                completed_subagents,
            );
        }
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

/// Fold one [`SubagentUpdate`] into the matching entry of `subagent_children`
/// (looked up by `handle_id`, matching the `AgentEvent::SubagentStarted` that
/// created it). `Finished`/`Failed` remove the entry and hand it to
/// [`finalize_subagent_child`], which embeds the completed child trajectory
/// into the parent's own trace right away — see [`StepState::completed_subagents`].
///
/// If no entry exists for `handle_id` (e.g. a `SubagentEvent` arriving before
/// its `SubagentStarted`, which should not happen given how the `task` tool
/// emits these, but the map lookup is defensive either way), the update is
/// logged and otherwise ignored rather than panicking.
///
/// `call_id` is the spawning `task` tool call's id (the same one
/// `AgentEvent::ToolCallStarted`/`ToolCallFinished` carry for that call) —
/// threaded through to [`finalize_subagent_child`] so the embedded-trajectory
/// observation can be correlated with that `tool_calls` entry; see
/// [`StepAssembler::push_subagent_embedded`]'s doc comment for why that
/// correlation matters.
fn handle_subagent_update(
    call_id: &str,
    handle_id: String,
    update: SubagentUpdate,
    subagent_children: &mut HashMap<String, SubagentChildState>,
    parent_assembler: &mut StepAssembler,
    completed_subagents: &mut Vec<Trajectory>,
) {
    match update {
        SubagentUpdate::TextDelta(delta) => {
            if let Some(child) = subagent_children.get_mut(&handle_id) {
                child.text_buf.push_str(&delta);
            }
        }
        SubagentUpdate::ThinkingDelta(delta) => {
            if let Some(child) = subagent_children.get_mut(&handle_id) {
                child.thinking_buf.push_str(&delta);
            }
        }
        SubagentUpdate::ToolCallStarted { id, name, args } => {
            let Some(child) = subagent_children.get_mut(&handle_id) else {
                return;
            };
            // A tool call always closes whatever thinking/text was buffered
            // for the turn so far — mirrors the parent's own
            // thinking-then-tool-call ordering (see `StepAssembler::push_thinking`'s
            // doc comment).
            if !child.thinking_buf.is_empty() {
                child.assembler.push_thinking(&child.thinking_buf);
                child.thinking_buf.clear();
            }
            if !child.text_buf.is_empty() {
                child
                    .assembler
                    .push_message(&Message::assistant(&child.text_buf));
                child.text_buf.clear();
            }
            write_stderr(&format!(
                "[sven:subagent:tool:call] handle_id=\"{handle_id}\" id=\"{id}\" name=\"{name}\" args={}",
                serde_json::to_string(&args).unwrap_or_default()
            ));
            let arguments = serde_json::to_string(&args).unwrap_or_default();
            child.assembler.push_message(&Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: id,
                    function: FunctionCall { name, arguments },
                },
            });
        }
        SubagentUpdate::ToolCallFinished {
            id,
            name,
            output,
            is_error,
        } => {
            let Some(child) = subagent_children.get_mut(&handle_id) else {
                return;
            };
            write_stderr(&format!(
                "[sven:subagent:tool:result] handle_id=\"{handle_id}\" id=\"{id}\" name=\"{name}\" success={}",
                !is_error
            ));
            child
                .assembler
                .push_message(&Message::tool_result(&id, &output));
        }
        SubagentUpdate::Finished { final_text } => {
            let Some(child) = subagent_children.remove(&handle_id) else {
                return;
            };
            write_stderr(&format!(
                "[sven:subagent:finished] handle_id=\"{handle_id}\""
            ));
            finalize_subagent_child(
                call_id,
                &handle_id,
                child,
                Some(final_text),
                None,
                parent_assembler,
                completed_subagents,
            );
        }
        SubagentUpdate::Failed { reason } => {
            let Some(child) = subagent_children.remove(&handle_id) else {
                return;
            };
            write_stderr(&format!(
                "[sven:subagent:failed] handle_id=\"{handle_id}\" reason={reason:?}"
            ));
            finalize_subagent_child(
                call_id,
                &handle_id,
                child,
                None,
                Some(reason),
                parent_assembler,
                completed_subagents,
            );
        }
        // Per-subagent cost reporting has no natural home on a `TraceStep`
        // yet (the child's `Trajectory.final_metrics` would be the right
        // place, but populating it needs token counts this event doesn't
        // carry, only `cost_usd`) — left as a follow-up; not folded into the
        // trace, silently ignored like the parent run's own per-event cost
        // deltas (which only ever surface via the aggregate `[sven:tokens]`
        // line, not per-turn).
        SubagentUpdate::TokenUsage { .. } => {}
    }
}

/// Finish a [`SubagentChildState`]'s assembler into a standalone
/// [`Trajectory`] and embed it into the parent's trace: attaches an
/// embedded-trajectory observation to whatever step is currently pending on
/// `parent_assembler` ([`StepAssembler::push_subagent_embedded`], correlated
/// with `call_id`, the spawning `task` call) and appends the child
/// trajectory to `completed_subagents` for the next flush to attach to
/// `Trajectory.subagent_trajectories`.
///
/// Exactly one of `final_text` / `failure_reason` is expected to be `Some`
/// (`Finished` vs. `Failed`); if a failure reason is given it is recorded as
/// a trailing assistant note so the partial trajectory still explains why it
/// stopped.
fn finalize_subagent_child(
    call_id: &str,
    handle_id: &str,
    child: SubagentChildState,
    final_text: Option<String>,
    failure_reason: Option<String>,
    parent_assembler: &mut StepAssembler,
    completed_subagents: &mut Vec<Trajectory>,
) {
    let SubagentChildState {
        trajectory_id,
        mut assembler,
        text_buf,
        thinking_buf,
    } = child;

    if !thinking_buf.is_empty() {
        assembler.push_thinking(&thinking_buf);
    }
    // Prefer whatever was actually streamed (`text_buf`); `final_text` is
    // the ACP-reported "accumulated assistant response" and is only used as
    // a fallback for a mock/driver that reports `Finished` without ever
    // streaming `TextDelta`s first.
    let closing_text = if !text_buf.is_empty() {
        text_buf
    } else {
        final_text.unwrap_or_default()
    };
    if !closing_text.is_empty() {
        assembler.push_message(&Message::assistant(&closing_text));
    }
    if let Some(reason) = failure_reason {
        assembler.push_message(&Message::assistant(format!("(subagent failed: {reason})")));
    }

    let mut child_trajectory = Trajectory::new(
        trace_session::ATIF_SCHEMA_VERSION,
        trace_session::default_agent_profile(),
    );
    child_trajectory.trajectory_id = Some(trajectory_id.clone());
    child_trajectory.steps = assembler.finish();

    parent_assembler.push_subagent_embedded(Some(call_id), &trajectory_id, None);
    completed_subagents.push(child_trajectory);

    write_stderr(&format!(
        "[sven:subagent:embedded] handle_id=\"{handle_id}\" trajectory_id=\"{trajectory_id}\""
    ));
}
