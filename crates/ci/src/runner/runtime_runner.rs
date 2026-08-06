// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Kernel-based CI runner that drives a single session to completion.
//!
//! This is the HSM-era replacement for [`super::CiRunner`]. It:
//!
//! 1. Builds a kernel [`SessionBundle`] via [`RuntimeBuilder::build_session`]
//!    in the reactive `agent` mode (the streaming, native-tool-calling coding
//!    agent), with auto-approval for all human gates (no interactive prompts
//!    in CI).
//! 2. Subscribes to the outward observation plane.
//! 3. Posts the initial [`Event::UserMessage`] from the step prompt.
//! 4. Bridges every [`UiEvent`] to CI output: assistant text → stdout,
//!    diagnostics (thinking, tool progress, usage, errors) → stderr.
//! 5. Returns exit code 0 when the turn completes, non-zero on error/timeout.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use tokio::sync::broadcast::error::RecvError;

use sven_bootstrap::{KernelChannels, RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, Config};
use sven_hsm::{Event, EventSink, UiEvent};
use sven_model::Message;

use crate::output::{
    finalise_stdout, format_token_usage_line, write_progress, write_stderr, write_stdout,
};
use crate::runner::{EXIT_AGENT_ERROR, EXIT_BUDGET_EXHAUSTED, EXIT_SUCCESS, EXIT_TIMEOUT};

// ── RuntimeRunner ─────────────────────────────────────────────────────────────

/// Headless CI runner backed by the HSM kernel.
///
/// Runs exactly one prompt and waits for the reactive agent turn to finish,
/// auto-approving any tool-permission gates. Multi-step workflow / JSONL
/// piping continues to live in [`super::CiRunner`]; this runner is the
/// single-prompt kernel path.
pub struct RuntimeRunner {
    config: Arc<Config>,
}

/// Options for a single kernel-driven CI run.
#[derive(Debug)]
pub struct RuntimeRunnerOptions {
    /// Mode string (e.g. `"agent"`, `"chat"`, `"sdlc"`) — selects which kernel
    /// machine runs. `Self::kernel_mode` collapses `Plan`/`Research`/`Agent`
    /// to the same "agent" machine string; `agent_mode` below is what
    /// actually differentiates their PERMISSION policy.
    pub mode: String,
    /// The original interactive [`AgentMode`], passed to
    /// `RuntimeBuilder::with_agent_mode`. Without this, `--mode plan`/
    /// `--mode research` silently ran with full write permissions through
    /// this runner (confirmed against a real packaged build before this fix)
    /// -- see the identical field on `crate::kernel_agent::KernelAgent`.
    pub agent_mode: AgentMode,
    /// The single user prompt to execute.
    pub prompt: String,
    /// Prior conversation history to seed into the kernel thread before the
    /// prompt is posted. Used to replay a piped prior-conversation document
    /// (`sven '…' | sven 'next task'`) as context. Empty for a fresh run.
    pub history: Vec<Message>,
    /// Project root (used for checkpoint path, audit log, etc.).
    pub project_root: Option<std::path::PathBuf>,
    /// Per-run timeout in seconds. `None` means unlimited. When unset, the
    /// per-step timeout (if any) is used as the effective bound.
    pub timeout_secs: Option<u64>,
    /// Per-step timeout in seconds. For a single-turn kernel run this acts as
    /// a fallback bound when no per-run timeout is supplied.
    pub step_timeout_secs: Option<u64>,
    /// Cumulative token budget for the run. When the running total of input +
    /// output tokens reaches this value the run stops with
    /// [`EXIT_BUDGET_EXHAUSTED`]. `None` means unlimited (mirrors the legacy
    /// runner's `--max-tokens`).
    pub max_tokens_budget: Option<u64>,
    /// Extra text appended to the composed system prompt (mirrors the legacy
    /// runner's `--append-system-prompt`). `None` leaves the prompt unchanged.
    pub append_system_prompt: Option<String>,
    /// Suppress Sven's built-in system prompt (`--no-system`). See
    /// [`sven_core::AgentRuntimeContext::build_system_message`] for exact
    /// semantics when combined with `append_system_prompt`.
    pub no_system: bool,
    /// Verbosity level (0 = minimal, 1 = verbose, 2+ = trace).
    pub trace_level: u8,
}

impl RuntimeRunner {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    /// Map a caller mode string to a registered kernel mode. Unknown / coding
    /// modes resolve to the reactive `agent` machine.
    fn kernel_mode(mode: &str) -> &'static str {
        match mode {
            "chat" => "chat",
            "sdlc" => "sdlc",
            _ => "agent",
        }
    }

    /// Run the kernel to completion and return an exit code.
    ///
    /// - `0` → the turn completed.
    /// - `1` → an error occurred.
    /// - `124` → timeout expired before completion.
    pub async fn run(&self, opts: RuntimeRunnerOptions) -> i32 {
        match self.run_inner(opts).await {
            Ok(code) => code,
            Err(e) => {
                write_stderr(&format!("[sven:fatal] RuntimeRunner failed: {e:#}"));
                EXIT_AGENT_ERROR
            }
        }
    }

    async fn run_inner(&self, opts: RuntimeRunnerOptions) -> anyhow::Result<i32> {
        // Discover the project context file (`.sven/context.md` or `AGENTS.md`)
        // and inject it, matching the legacy runner's behaviour so headless
        // agents pick up project instructions. Skipped under `--no-system`:
        // it is only ever consumed by the built-in system prompt (see
        // `AgentRuntimeContext::build_system_message`), which `--no-system`
        // bypasses entirely, so reading and logging it would be pure waste.
        let project_context = if opts.no_system {
            None
        } else {
            opts.project_root
                .as_ref()
                .and_then(|r| sven_runtime::load_project_context_file_with_path(r))
        };
        if let Some((path, _)) = &project_context {
            write_stderr(&format!(
                "[sven:info] Project context file loaded from {}",
                path.display()
            ));
        }

        let runtime_ctx = RuntimeContext {
            project_root: opts.project_root.clone(),
            git_context: opts
                .project_root
                .as_ref()
                .map(|r| sven_runtime::collect_git_context(r)),
            ci_context: Some(crate::context::detect_ci_context()),
            project_context_file: project_context.map(|(_, content)| content),
            append_system_prompt: opts.append_system_prompt.clone(),
            system_prompt_override: self.config.agent.system_prompt.clone(),
            no_system: opts.no_system,
            skills: sven_runtime::SharedSkills::new(sven_runtime::discover_skills(
                opts.project_root.as_deref(),
            )),
            agents: sven_runtime::SharedAgents::new(sven_runtime::discover_agents(
                opts.project_root.as_deref(),
            )),
            knowledge: sven_runtime::SharedKnowledge::new(sven_runtime::discover_knowledge(
                opts.project_root.as_deref(),
            )),
            knowledge_drift_note: None,
        };

        let kernel_mode = Self::kernel_mode(&opts.mode);
        write_progress(&format!(
            "[sven:runtime-runner] mode={kernel_mode} prompt_len={}",
            opts.prompt.len()
        ));

        // Replay any piped prior-conversation history as kernel thread context
        // before the new prompt is posted (mirrors the legacy runner's
        // `[sven:info] Loaded N prior message(s)` notice).
        let history = opts.history.clone();
        if !history.is_empty() {
            write_progress(&format!(
                "[sven:info] Loaded {} prior message(s) into conversation history",
                history.len()
            ));
        }

        let bundle = RuntimeBuilder::new(self.config.clone(), kernel_mode)
            .with_runtime_context(runtime_ctx)
            .with_agent_mode(opts.agent_mode)
            .with_allow_interactive_oauth(false)
            .with_initial_history(history)
            .build_session()
            .await
            .context("failed to build kernel session")?;

        let sink: EventSink = bundle.handle.sink();
        let mut obs_rx = bundle.handle.subscribe_observations();

        // Auto-approve all human gates (CI is non-interactive).
        tokio::spawn(auto_approve(bundle.channels));

        // Post the user prompt.
        if !sink
            .emit(Event::UserMessage {
                text: opts.prompt.clone(),
            })
            .await
        {
            anyhow::bail!("kernel event queue closed before UserMessage was delivered");
        }

        let trace = opts.trace_level;
        let token_budget = opts.max_tokens_budget;
        let prompt = opts.prompt.clone();
        let drive = async {
            let mut state = CiOutState {
                trace,
                streamed_text: String::new(),
                sven_header_emitted: false,
                had_error: false,
                tools_used: 0,
                run_total_tokens: 0,
                token_budget,
            };

            // Emit the step banner and the `## User` conversation section up
            // front, mirroring the legacy conversation output contract.
            write_progress("[sven:step:start] 1/1");
            if !prompt.is_empty() {
                write_stdout(&format!("## User\n{prompt}\n\n"));
            }
            let started = Instant::now();

            let exit = loop {
                match obs_rx.recv().await {
                    Ok(ev) => {
                        if let Some(done) = handle_ui_event(ev, &mut state) {
                            break done;
                        }
                    }
                    // The session ended and dropped the sender; treat as done.
                    // Close any open `## Sven` streaming section first so the
                    // trailing conversation document is well-formed for a
                    // downstream pipe stage (mirrors the TurnComplete / Aborted
                    // finalisation paths).
                    Err(RecvError::Closed) => {
                        close_sven_section(&mut state);
                        finalise_stdout(&state.streamed_text);
                        break if state.had_error {
                            EXIT_AGENT_ERROR
                        } else {
                            EXIT_SUCCESS
                        };
                    }
                    // Dropped some observations under load; keep going.
                    Err(RecvError::Lagged(_)) => continue,
                }
            };

            write_progress(&format!(
                "[sven:step:complete] 1/1 duration_ms={} tools={} success={}",
                started.elapsed().as_millis(),
                state.tools_used,
                exit == EXIT_SUCCESS
            ));
            exit
        };

        // A per-run timeout takes precedence; otherwise fall back to the
        // per-step timeout (for a single-turn kernel run the two coincide).
        let effective_timeout = opts.timeout_secs.or(opts.step_timeout_secs);
        let result = if let Some(t) = effective_timeout {
            tokio::select! {
                r = drive => r,
                _ = tokio::time::sleep(Duration::from_secs(t)) => {
                    write_stderr(&format!("[sven:error] RuntimeRunner timed out after {t}s"));
                    EXIT_TIMEOUT
                }
            }
        } else {
            drive.await
        };

        // Keep the runtime alive until here so the audit log flushes.
        drop(bundle.runtime);
        Ok(result)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Mutable state threaded through [`handle_ui_event`] for one CI turn.
struct CiOutState {
    /// Verbosity level (0 = default, 1 = `-v`, 2+ = trace).
    trace: u8,
    /// Assistant text streamed to stdout so far this turn.
    streamed_text: String,
    /// Whether an open `## Sven` section is awaiting a close.
    sven_header_emitted: bool,
    /// Set when a fatal (non-tool) error was observed.
    had_error: bool,
    /// Number of tool calls issued this turn.
    tools_used: u32,
    /// Cumulative input + output tokens seen so far this run.
    run_total_tokens: u64,
    /// Optional cumulative token budget; the run stops once
    /// `run_total_tokens` reaches it. `None` means unlimited.
    token_budget: Option<u64>,
}

/// Build the verbose-only ` output=…` snippet for a `[sven:tool:result]` line.
///
/// Returns an empty string at default verbosity (`trace == 0`) or for empty
/// output, so the snippet appears only at `-v`. Long output is truncated to a
/// fixed character limit with a `…[+N chars]` suffix so a large tool payload
/// never floods stderr.
fn tool_output_snippet(trace: u8, output: &str) -> String {
    const TOOL_OUTPUT_SNIPPET_LIMIT: usize = 1500;
    if trace < 1 || output.is_empty() {
        return String::new();
    }
    let preview: String = output.chars().take(TOOL_OUTPUT_SNIPPET_LIMIT).collect();
    let total = output.chars().count();
    if total > TOOL_OUTPUT_SNIPPET_LIMIT {
        format!(" output={:?}...[+{} chars]", preview, total - TOOL_OUTPUT_SNIPPET_LIMIT)
    } else {
        format!(" output={output:?}")
    }
}

/// Close an open `## Sven` streaming section, if any.
fn close_sven_section(state: &mut CiOutState) {
    if state.sven_header_emitted {
        if state.streamed_text.ends_with('\n') {
            write_stdout("\n");
        } else {
            write_stdout("\n\n");
        }
        state.sven_header_emitted = false;
    }
}

/// Bridge a single [`UiEvent`] to CI output.
///
/// stderr carries the structured `[sven:*]` diagnostic trace; stdout carries
/// the pipeable conversation document (`## User` / `## Sven` / `## Tool` /
/// `## Tool Result`). This mirrors the contract emitted by the legacy runner's
/// [`handle_event`](super::event::handle_event) so headless output is stable
/// across the two backends.
///
/// Returns `Some(exit_code)` when the turn has settled (the caller should stop
/// driving the loop), or `None` to keep going.
fn handle_ui_event(ev: UiEvent, state: &mut CiOutState) -> Option<i32> {
    match ev {
        UiEvent::TextDelta(d) => {
            if !state.sven_header_emitted {
                write_stdout("## Sven\n");
                state.sven_header_emitted = true;
            }
            state.streamed_text.push_str(&d);
            write_stdout(&d);
        }
        // Deltas were already streamed; only emit the complete text if nothing
        // streamed (some providers send a single TextComplete with no deltas).
        UiEvent::TextComplete(t) => {
            if state.streamed_text.is_empty() && !t.is_empty() {
                write_stdout("## Sven\n");
                state.streamed_text.push_str(&t);
                write_stdout(&t);
                state.sven_header_emitted = true;
            }
            close_sven_section(state);
        }
        UiEvent::ThinkingDelta(_) => {}
        // Thinking is high-value CI signal: emit the full content at every
        // verbosity level (only stderr, never stdout).
        UiEvent::ThinkingComplete(c) => {
            if !c.is_empty() {
                write_progress(&format!("[sven:thinking] {c}"));
            }
        }
        UiEvent::ToolStarted {
            call_id,
            name,
            args,
        } => {
            // A tool call ends any open assistant text section.
            close_sven_section(state);
            state.tools_used += 1;
            let args_str = serde_json::to_string(&args).unwrap_or_default();
            write_progress(&format!(
                "[sven:tool:call] id=\"{call_id}\" name=\"{name}\" args={args_str}"
            ));
            let envelope = serde_json::json!({
                "tool_call_id": call_id,
                "name": name,
                "args": args,
            });
            let pretty = serde_json::to_string_pretty(&envelope).unwrap_or_default();
            write_stdout(&format!("## Tool\n```json\n{pretty}\n```\n\n"));
        }
        UiEvent::ToolProgress { message, .. } => {
            if state.trace >= 1 {
                write_progress(&format!("[sven:progress] {message}"));
            }
        }
        UiEvent::ToolFinished {
            call_id,
            name,
            is_error,
            output,
        } => {
            // The `output=` snippet is verbose-only signal for both success and
            // error outcomes: it appears solely at `-v` (trace >= 1) and is
            // truncated, so a large tool payload never floods stderr at default
            // verbosity. Tool errors are non-fatal — the model receives the
            // error and may recover on the next round, so the run still
            // succeeds; only the `success=<bool>` field distinguishes them.
            let success = !is_error;
            let output_snippet = tool_output_snippet(state.trace, &output);
            write_progress(&format!(
                "[sven:tool:result] id=\"{call_id}\" name=\"{name}\" success={success} size={}{}",
                output.len(),
                output_snippet
            ));
            write_stdout(&format!("## Tool Result\n```\n{output}\n```\n\n"));
        }
        UiEvent::TokenUsage {
            input,
            output,
            cache_read,
            cache_write,
            cache_read_total,
            cache_write_total,
            max_tokens,
            max_output_tokens,
            ..
        } => {
            write_progress(&format_token_usage_line(
                input,
                output,
                cache_read,
                cache_write,
                cache_read_total,
                cache_write_total,
                max_tokens,
                max_output_tokens,
            ));
            // Enforce the cumulative token budget (mirrors the legacy runner's
            // `--max-tokens`). Stop the run when the running total reaches it.
            state.run_total_tokens += u64::from(input) + u64::from(output);
            if let Some(budget) = state.token_budget {
                if budget > 0 && state.run_total_tokens >= budget {
                    write_stderr(&format!(
                        "[sven:error] Token budget exhausted: {} tokens used (budget: {budget}). Stopping.",
                        state.run_total_tokens
                    ));
                    close_sven_section(state);
                    finalise_stdout(&state.streamed_text);
                    return Some(EXIT_BUDGET_EXHAUSTED);
                }
            }
        }
        UiEvent::ContextCompacted {
            tokens_before,
            tokens_after,
            strategy,
            turn,
        } => {
            write_progress(&format!(
                "[sven:compact] strategy={strategy} turn={turn} {tokens_before}->{tokens_after}"
            ));
        }
        UiEvent::TodoUpdate(_) => {}
        UiEvent::ModeChanged(m) => write_progress(&format!("[sven:mode] {m}")),
        UiEvent::ModelChanged(m) => write_progress(&format!("[sven:model] {m}")),
        UiEvent::Transition { from, to, event } => {
            if state.trace >= 2 {
                write_progress(&format!("[sven:transition] {from} -> {to} on {event}"));
            }
        }
        UiEvent::Error(e) => {
            state.had_error = true;
            write_stderr(&format!("[sven:error] {e}"));
        }
        UiEvent::TurnComplete => {
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            return Some(if state.had_error {
                EXIT_AGENT_ERROR
            } else {
                EXIT_SUCCESS
            });
        }
        UiEvent::Aborted { partial_text } => {
            if state.streamed_text.is_empty() && !partial_text.is_empty() {
                write_stdout("## Sven\n");
                write_stdout(&partial_text);
                state.streamed_text.push_str(&partial_text);
                state.sven_header_emitted = true;
            }
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            return Some(EXIT_SUCCESS);
        }
        // Subagent / delegate / team observations are otherwise rendered as
        // rich child-session views by the interactive frontends; this runner
        // has no such surface (and, unlike `CiRunner`, no ATIF trace
        // document to embed a subagent trajectory into — `RuntimeRunner`
        // never writes `--output-trace`; see `main.rs`'s runner-routing
        // comment, which forces `CiRunner` whenever `--output-trace` is
        // given). Still emit the same `[sven:subagent:...]` stderr tokens
        // `CiRunner` does (see `crates/ci/src/runner/event.rs`) so a plain
        // `sven --headless` run (no `--output-trace`) has the same
        // subagent-lifecycle visibility on stderr instead of silently
        // dropping these — previously the case for every field here.
        UiEvent::SubagentStarted {
            call_id,
            handle_id,
            description,
            ..
        } => {
            write_stderr(&format!(
                "[sven:subagent:started] call_id=\"{call_id}\" handle_id=\"{handle_id}\" description={description:?}"
            ));
        }
        UiEvent::SubagentEvent { handle_id, update, .. } => {
            if let Ok(update) = serde_json::from_value::<sven_tools::events::SubagentUpdate>(update) {
                match update {
                    sven_tools::events::SubagentUpdate::Finished { .. } => {
                        write_stderr(&format!("[sven:subagent:finished] handle_id=\"{handle_id}\""));
                    }
                    sven_tools::events::SubagentUpdate::Failed { reason } => {
                        write_stderr(&format!(
                            "[sven:subagent:failed] handle_id=\"{handle_id}\" reason={reason:?}"
                        ));
                    }
                    _ => {}
                }
            }
        }
        UiEvent::DelegateSummary {
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
        UiEvent::CollabEvent(_) | UiEvent::PeerList(_) => {}
    }
    None
}

/// Auto-approve every human gate, replying immediately so CI never blocks.
async fn auto_approve(mut channels: KernelChannels) {
    loop {
        tokio::select! {
            q = channels.question_rx.recv() => match q {
                // No interactive user in CI: reply with an empty answer.
                Some(q) => { let _ = q.reply_tx.send(String::new()); }
                None => break,
            },
            a = channels.approval_rx.recv() => match a {
                // Approve all tool capabilities in CI.
                Some(a) => { let _ = a.reply_tx.send(true); }
                None => break,
            },
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_runner_options_debug() {
        let opts = RuntimeRunnerOptions {
            mode: "agent".into(),
            agent_mode: AgentMode::Agent,
            prompt: "hello".into(),
            history: Vec::new(),
            project_root: None,
            timeout_secs: Some(30),
            step_timeout_secs: None,
            max_tokens_budget: None,
            append_system_prompt: None,
            no_system: false,
            trace_level: 0,
        };
        assert!(format!("{opts:?}").contains("agent"));
    }

    #[test]
    fn kernel_mode_maps_coding_modes_to_agent() {
        assert_eq!(RuntimeRunner::kernel_mode("code"), "agent");
        assert_eq!(RuntimeRunner::kernel_mode("plan"), "agent");
        assert_eq!(RuntimeRunner::kernel_mode("research"), "agent");
        assert_eq!(RuntimeRunner::kernel_mode("chat"), "chat");
        assert_eq!(RuntimeRunner::kernel_mode("sdlc"), "sdlc");
    }

    fn state(trace: u8) -> CiOutState {
        CiOutState {
            trace,
            streamed_text: String::new(),
            sven_header_emitted: false,
            had_error: false,
            tools_used: 0,
            run_total_tokens: 0,
            token_budget: None,
        }
    }

    #[test]
    fn text_delta_streams_to_buffer() {
        let mut st = state(0);
        let r = handle_ui_event(UiEvent::TextDelta("pong".into()), &mut st);
        assert!(r.is_none());
        assert_eq!(st.streamed_text, "pong");
        assert!(!st.had_error);
        assert!(st.sven_header_emitted);
    }

    #[test]
    fn turn_complete_returns_success() {
        let mut st = state(0);
        st.streamed_text.push_str("pong");
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_SUCCESS));
    }

    #[test]
    fn error_then_turn_complete_returns_agent_error() {
        let mut st = state(0);
        handle_ui_event(UiEvent::Error("boom".into()), &mut st);
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_AGENT_ERROR));
    }

    #[test]
    fn tool_started_counts_and_traces() {
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::ToolStarted {
                call_id: "tc-1".into(),
                name: "write_file".into(),
                args: serde_json::json!({"path": "/tmp/x"}),
            },
            &mut st,
        );
        assert!(r.is_none());
        assert_eq!(st.tools_used, 1);
    }

    fn token_usage(input: u32, output: u32) -> UiEvent {
        UiEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total: 0,
            cache_write_total: 0,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
        }
    }

    #[test]
    fn token_budget_exhaustion_stops_run() {
        let mut st = state(0);
        st.token_budget = Some(100);
        // First update stays under budget.
        assert_eq!(handle_ui_event(token_usage(40, 10), &mut st), None);
        assert_eq!(st.run_total_tokens, 50);
        // Second update crosses the budget → run stops with the budget code.
        let r = handle_ui_event(token_usage(40, 20), &mut st);
        assert_eq!(r, Some(EXIT_BUDGET_EXHAUSTED));
    }

    #[test]
    fn token_usage_without_budget_never_stops() {
        let mut st = state(0);
        assert_eq!(handle_ui_event(token_usage(1_000, 1_000), &mut st), None);
        assert_eq!(st.run_total_tokens, 2_000);
    }

    #[test]
    fn output_snippet_gated_by_verbosity() {
        // Default verbosity: no snippet regardless of outcome.
        assert_eq!(tool_output_snippet(0, "some output"), "");
        // Verbose: snippet present.
        assert!(tool_output_snippet(1, "some output").contains("output="));
        // Empty output: never a snippet even at -v.
        assert_eq!(tool_output_snippet(1, ""), "");
    }

    #[test]
    fn output_snippet_truncates_long_output() {
        let long = "x".repeat(4000);
        let snippet = tool_output_snippet(1, &long);
        assert!(snippet.contains("...[+2500 chars]"));
    }

    #[test]
    fn tool_error_is_non_fatal() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::ToolFinished {
                call_id: "tc-1".into(),
                name: "read_file".into(),
                output: "no such file".into(),
                is_error: true,
            },
            &mut st,
        );
        // A tool error must not mark the whole turn as failed.
        assert!(!st.had_error);
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_SUCCESS));
    }
}
