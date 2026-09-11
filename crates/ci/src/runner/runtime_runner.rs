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

use atif::Trajectory;
use sven_bootstrap::{RuntimeBuilder, RuntimeContext};
use sven_config::{AgentMode, Config};
use sven_hsm::{Event, UiEvent};
use sven_kernel::EventSink;
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_session_store::trace_session::{self, StepAssembler, SvenSessionMeta};
use sven_session_store::{apply_reward_to_trajectory, make_title, OutcomeFold, RunConclusion};
use sven_tools::ToolCall;

use crate::output::{
    finalise_stdout, format_token_usage_line, tool_output_snippet, write_progress, write_stderr,
    write_stdout,
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
    /// [`sven_machines::AgentRuntimeContext::build_system_message`] for exact
    /// semantics when combined with `append_system_prompt`.
    pub no_system: bool,
    /// Disable all tools for this session (`--no-tools`): no tool schemas are
    /// sent to the model and any tool call is refused.
    pub no_tools: bool,
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
                .and_then(|r| sven_workspace::find_project_context_file(r))
        };
        if let Some(path) = &project_context {
            write_stderr(&format!(
                "[sven:info] Project context file found at {} (referenced, not auto-loaded)",
                path.display()
            ));
        }

        let runtime_ctx = RuntimeContext {
            project_context_file: project_context,
            append_system_prompt: opts.append_system_prompt.clone(),
            system_prompt_override: self.config.agent.system_prompt.clone(),
            no_system: opts.no_system,
            no_tools: opts.no_tools,
            ..RuntimeContext::auto_detect_at(opts.project_root.clone())
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
        tokio::spawn(bundle.channels.auto_approve());

        // Post the user prompt.
        if !sink
            .emit(Event::UserMessage {
                text: opts.prompt.clone(),
            })
            .await
        {
            anyhow::bail!("kernel event queue closed before UserMessage was delivered");
        }

        // The trajectory this run records: history replayed into the kernel
        // first, then the new prompt, so the document stands on its own.
        let mut assembler = StepAssembler::new();
        for msg in &opts.history {
            assembler.push_message(msg);
        }
        assembler.push_message(&Message::user(&opts.prompt));

        let mut state = CiOutState {
            trace: opts.trace_level,
            prompt: opts.prompt.clone(),
            user_header_emitted: false,
            streamed_text: String::new(),
            sven_header_emitted: false,
            had_error: false,
            tools_used: 0,
            run_total_tokens: 0,
            token_budget: opts.max_tokens_budget,
            total_input: 0,
            total_output: 0,
            latest_cache_read_total: 0,
            latest_cache_write_total: 0,
            assembler,
            outcome: OutcomeFold::default(),
        };

        // The step banner goes to stderr immediately; the `## User`
        // conversation section on stdout is written lazily (see
        // `ensure_user_header`) - only once the turn is about to
        // produce its first real stdout content. A turn that fails
        // before producing anything must leave stdout untouched
        // (`11_error_handling.bats`'s "produces no stdout content"),
        // and eagerly writing "## User" here (as this used to)
        // unconditionally broke that contract on every provider error.
        write_progress("[sven:step:start] 1/1");
        let started = Instant::now();

        // A per-run timeout takes precedence; otherwise fall back to the
        // per-step timeout (for a single-turn kernel run the two coincide).
        // The deadline is enforced per `recv` rather than by racing the whole
        // drive loop in a `select!`: dropping that future on timeout would
        // take `state` - and with it the trajectory accumulated so far - down
        // with it, leaving a timed-out run with no record at all.
        let deadline = opts
            .timeout_secs
            .or(opts.step_timeout_secs)
            .map(|t| started + Duration::from_secs(t));

        let result = loop {
            let received = match deadline {
                Some(dl) => {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    match tokio::time::timeout(remaining, obs_rx.recv()).await {
                        Ok(r) => r,
                        Err(_) => {
                            write_stderr(&format!(
                                "[sven:error] RuntimeRunner timed out after {}s",
                                (dl - started).as_secs()
                            ));
                            break EXIT_TIMEOUT;
                        }
                    }
                }
                None => obs_rx.recv().await,
            };
            match received {
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
                    print_total_usage(&state);
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
            result == EXIT_SUCCESS
        ));

        write_trajectory(state, result, &self.config, &opts.mode);

        // Keep the runtime alive until here so the audit log flushes.
        bundle.runtime.detach();
        Ok(result)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Mutable state threaded through [`handle_ui_event`] for one CI turn.
struct CiOutState {
    /// Verbosity level (0 = default, 1 = `-v`, 2+ = trace).
    trace: u8,
    /// The user prompt, held so `ensure_user_header` can write it lazily
    /// (see that function's doc comment for why eager doesn't work).
    prompt: String,
    /// Whether the `## User` section has been written yet this turn.
    user_header_emitted: bool,
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
    /// Cumulative input tokens across every turn this run.
    total_input: u64,
    /// Cumulative output tokens across every turn this run.
    total_output: u64,
    /// Most recently reported cache_read_total/cache_write_total (already
    /// cumulative across turns - see `TurnExecutor`'s per-thread cache
    /// tracking - so the latest value IS the run total, nothing to sum here).
    latest_cache_read_total: u32,
    latest_cache_write_total: u32,
    /// Accumulates this run's `TraceStep`s for the ATIF trajectory written at
    /// conclusion. Fed with the same message shapes the legacy runner uses
    /// (see [`super::event::handle_event`]) so both backends produce
    /// interchangeable documents.
    assembler: StepAssembler,
    /// Outcome tally for the reward stamped on that trajectory. Reward-only:
    /// the exit code is still decided by `had_error` and the budget/timeout
    /// paths alone.
    outcome: OutcomeFold,
}

/// Map this runner's exit code onto the conclusion the reward scorer expects.
///
/// `EXIT_SUCCESS` is a claim, not a verdict: an aborted run also exits 0 here,
/// and [`OutcomeFold::conclude`] vetoes it from the event stream.
fn conclusion_for(exit: i32) -> RunConclusion {
    match exit {
        EXIT_SUCCESS => RunConclusion::Success,
        EXIT_TIMEOUT => RunConclusion::Timeout,
        EXIT_BUDGET_EXHAUSTED => RunConclusion::BudgetExhausted,
        _ => RunConclusion::AgentError,
    }
}

/// Write this run's ATIF trajectory to the project auto-log, stamped with the
/// reward its outcome earned.
///
/// Only ever called once, after the run has settled, so the document is always
/// a conclusion — a consumer that sees no `final_metrics.extra.reward` on a
/// trajectory knows the session did not finish.
///
/// Writes nothing when there is no `.sven/` project directory to write into
/// (`resolve_auto_log_path` returns `None`), and nothing here is fatal: a
/// failed trace write warns and leaves the run's exit code alone. Unlike
/// `CiRunner`, this runner embeds no `subagent_trajectories` — it keeps no
/// child assemblers.
fn write_trajectory(state: CiOutState, exit: i32, config: &Config, mode: &str) {
    let Some(path) = sven_workspace::resolve_auto_log_path() else {
        return;
    };
    let reward = state.outcome.conclude(conclusion_for(exit));

    let agent = trace_session::default_agent_profile()
        .with_model(format!("{}/{}", config.model.provider, config.model.name));
    let mut trajectory = Trajectory::new(trace_session::ATIF_SCHEMA_VERSION, agent);
    trajectory.session_id = Some(trace_session::new_session_id());
    trajectory.steps = state.assembler.finish();

    let mut meta = SvenSessionMeta::new(make_title(&state.prompt));
    meta.mode = Some(mode.to_string());
    meta.apply_to_trajectory(&mut trajectory);
    apply_reward_to_trajectory(&mut trajectory, &reward);

    match atif::persist::write_trajectory_atomic(&path, &trajectory, None) {
        Ok(()) => write_progress(&format!("[sven:trace] Trace written to {}", path.display())),
        Err(e) => write_stderr(&format!(
            "[sven:warn] Failed to write trace log {}: {e}",
            path.display()
        )),
    }
}

/// Write the `## User` conversation section exactly once, right before the
/// turn's first real stdout content (assistant text, a tool call, or a
/// partial answer from an abort).
///
/// Deliberately lazy, not eager: writing it unconditionally up front (as
/// this runner used to) means a turn that fails before producing anything
/// still leaves "## User\n<prompt>\n\n" on stdout, contradicting the
/// contract that a failed turn produces no stdout content at all
/// (`11_error_handling.bats`, "provider mid-stream error produces no stdout
/// content") - confirmed to reproduce on the original code with no relation
/// to any other change, i.e. a pre-existing bug independent of what's being
/// worked on here, still worth fixing under the same "always fix bugs you
/// find" instruction as everything else in this session.
fn ensure_user_header(state: &mut CiOutState) {
    if !state.user_header_emitted {
        if !state.prompt.is_empty() {
            write_stdout(&format!("## User\n{}\n\n", state.prompt));
        }
        state.user_header_emitted = true;
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

/// Print the run-wide `[sven:tokens:total]` summary once the run has
/// settled: total input/output tokens across every turn, and the latest
/// (already cumulative - see `TurnExecutor`) cache totals.
fn print_total_usage(state: &CiOutState) {
    write_progress(&format!(
        "[sven:tokens:total] input={} output={} cache_read={} cache_write={}",
        state.total_input,
        state.total_output,
        state.latest_cache_read_total,
        state.latest_cache_write_total
    ));
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
    // Fold into the reward tally before dispatching, so no arm can forget it.
    state.outcome.observe(&ev);
    match ev {
        UiEvent::TextDelta(d) => {
            if !state.sven_header_emitted {
                ensure_user_header(state);
                write_stdout("## Sven\n");
                state.sven_header_emitted = true;
            }
            state.streamed_text.push_str(&d);
            write_stdout(&d);
        }
        // Deltas were already streamed; only emit the complete text if nothing
        // streamed (some providers send a single TextComplete with no deltas).
        UiEvent::TextComplete(t) => {
            if !t.is_empty() {
                state.assembler.push_message(&Message::assistant(&t));
            }
            if state.streamed_text.is_empty() && !t.is_empty() {
                ensure_user_header(state);
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
                state.assembler.push_thinking(&c);
            }
        }
        UiEvent::ToolCallStarted(ToolCall {
            id: call_id,
            name,
            args,
        }) => {
            // A tool call ends any open assistant text section.
            close_sven_section(state);
            ensure_user_header(state);
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
            state.assembler.push_message(&Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: call_id,
                    function: FunctionCall {
                        name,
                        arguments: args_str,
                    },
                },
            });
        }
        UiEvent::ToolProgress { message, .. } => {
            if state.trace >= 1 {
                write_progress(&format!("[sven:progress] {message}"));
            }
        }
        UiEvent::ToolCallFinished {
            call_id,
            tool_name: name,
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
            state
                .assembler
                .push_message(&Message::tool_result(&call_id, &output));
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
            // Usage often arrives before TextComplete (many providers send
            // the usage chunk as the last SSE frame ahead of [DONE]). Close
            // any open `## Sven` streaming section first so this stderr
            // write doesn't land glued onto the end of the still-open
            // stdout text with no newline between them.
            close_sven_section(state);
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
            state.total_input += u64::from(input);
            state.total_output += u64::from(output);
            state.latest_cache_read_total = cache_read_total;
            state.latest_cache_write_total = cache_write_total;
            if let Some(budget) = state.token_budget {
                if budget > 0 && state.run_total_tokens >= budget {
                    write_stderr(&format!(
                        "[sven:error] Token budget exhausted: {} tokens used (budget: {budget}). Stopping.",
                        state.run_total_tokens
                    ));
                    close_sven_section(state);
                    finalise_stdout(&state.streamed_text);
                    print_total_usage(state);
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
            state.assembler.push_context_compacted(
                tokens_before,
                tokens_after,
                Some(&strategy.to_string()),
                Some(turn),
            );
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
            print_total_usage(state);
            return Some(if state.had_error {
                EXIT_AGENT_ERROR
            } else {
                EXIT_SUCCESS
            });
        }
        UiEvent::Aborted { partial_text } => {
            // Record whatever the model managed to say before the abort, on
            // the same condition stdout uses: only when nothing was streamed,
            // since a streamed turn ends in `TextComplete`, which already
            // pushed it. The run scores zero either way.
            if state.streamed_text.is_empty() && !partial_text.is_empty() {
                state
                    .assembler
                    .push_message(&Message::assistant(&partial_text));
            }
            if state.streamed_text.is_empty() && !partial_text.is_empty() {
                ensure_user_header(state);
                write_stdout("## Sven\n");
                write_stdout(&partial_text);
                state.streamed_text.push_str(&partial_text);
                state.sven_header_emitted = true;
            }
            close_sven_section(state);
            finalise_stdout(&state.streamed_text);
            print_total_usage(state);
            return Some(EXIT_SUCCESS);
        }
        // Subagent / delegate / team observations are otherwise rendered as
        // rich child-session views by the interactive frontends; this runner
        // has no such surface. It writes an ATIF trajectory (the project
        // auto-log; `--output-trace` still routes to `CiRunner`, see
        // `run/ci.rs`'s runner-routing comment) but keeps no child
        // assemblers, so that document carries no `subagent_trajectories` —
        // unlike `CiRunner`'s. Still emit the same `[sven:subagent:...]` stderr tokens
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
        UiEvent::SubagentEvent {
            handle_id, update, ..
        } => match update {
            sven_tools::events::SubagentUpdate::Finished { .. } => {
                write_stderr(&format!(
                    "[sven:subagent:finished] handle_id=\"{handle_id}\""
                ));
            }
            sven_tools::events::SubagentUpdate::Failed { reason } => {
                write_stderr(&format!(
                    "[sven:subagent:failed] handle_id=\"{handle_id}\" reason={reason:?}"
                ));
            }
            _ => {}
        },
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
        // Question/QuestionAnswer never fire in headless mode (the session
        // runs under an auto-approve gate); TitleGenerated has no headless
        // stdout/stderr representation.
        UiEvent::CollabEvent(_)
        | UiEvent::PeerList(_)
        | UiEvent::Question { .. }
        | UiEvent::QuestionAnswer { .. }
        | UiEvent::TitleGenerated(_) => {}
    }
    None
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
            no_tools: false,
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
            prompt: "test prompt".to_string(),
            user_header_emitted: false,
            streamed_text: String::new(),
            sven_header_emitted: false,
            had_error: false,
            tools_used: 0,
            run_total_tokens: 0,
            token_budget: None,
            total_input: 0,
            total_output: 0,
            latest_cache_read_total: 0,
            latest_cache_write_total: 0,
            assembler: StepAssembler::new(),
            outcome: OutcomeFold::default(),
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

    /// Regression test for the eager-`## User`-header bug: a turn that
    /// fails before producing any content must never write "## User" to
    /// stdout, matching `11_error_handling.bats`'s "produces no stdout
    /// content" contract. `ensure_user_header` is only called from the
    /// content-writing arms (`TextDelta`, `TextComplete`'s fallback,
    /// `ToolStarted`, `Aborted`-with-partial-text) - an error-only turn
    /// never reaches any of them, so `user_header_emitted` must stay false.
    #[test]
    fn error_only_turn_never_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(UiEvent::Error("boom".into()), &mut st);
        handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert!(
            !st.user_header_emitted,
            "an error-only turn must produce no stdout content at all"
        );
    }

    #[test]
    fn text_delta_emits_the_user_header_exactly_once() {
        let mut st = state(0);
        handle_ui_event(UiEvent::TextDelta("a".into()), &mut st);
        assert!(st.user_header_emitted);
        // A second delta must not re-emit it.
        handle_ui_event(UiEvent::TextDelta("b".into()), &mut st);
        assert!(st.user_header_emitted);
    }

    #[test]
    fn tool_started_emits_the_user_header_when_no_text_preceded_it() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "write_file".into(),
                args: serde_json::json!({}),
            }),
            &mut st,
        );
        assert!(st.user_header_emitted);
    }

    #[test]
    fn aborted_with_partial_text_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::Aborted {
                partial_text: "partial".into(),
            },
            &mut st,
        );
        assert!(st.user_header_emitted);
    }

    #[test]
    fn aborted_with_no_partial_text_never_emits_the_user_header() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::Aborted {
                partial_text: String::new(),
            },
            &mut st,
        );
        assert!(!st.user_header_emitted);
    }

    #[test]
    fn tool_started_counts_and_traces() {
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "write_file".into(),
                args: serde_json::json!({"path": "/tmp/x"}),
            }),
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

    fn token_usage_with_cache(
        input: u32,
        output: u32,
        cache_read_total: u32,
        cache_write_total: u32,
    ) -> UiEvent {
        UiEvent::TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            cache_read_total,
            cache_write_total,
            max_tokens: 0,
            max_output_tokens: 0,
            cost_usd: None,
        }
    }

    #[test]
    fn total_input_and_output_accumulate_across_turns() {
        let mut st = state(0);
        handle_ui_event(token_usage(40, 10), &mut st);
        handle_ui_event(token_usage(15, 7), &mut st);
        assert_eq!(st.total_input, 55);
        assert_eq!(st.total_output, 17);
    }

    #[test]
    fn latest_cache_totals_track_the_most_recent_event_not_a_sum() {
        // cache_read_total/cache_write_total on the event are ALREADY
        // cumulative (TurnExecutor's job) - CiOutState must track the latest
        // value, not sum successive already-cumulative numbers.
        let mut st = state(0);
        handle_ui_event(token_usage_with_cache(10, 2, 5, 3), &mut st);
        handle_ui_event(token_usage_with_cache(10, 2, 10, 6), &mut st);
        assert_eq!(st.latest_cache_read_total, 10);
        assert_eq!(st.latest_cache_write_total, 6);
    }

    #[test]
    fn tool_error_is_non_fatal() {
        let mut st = state(0);
        handle_ui_event(
            UiEvent::ToolCallFinished {
                call_id: "tc-1".into(),
                tool_name: "read_file".into(),
                output: "no such file".into(),
                is_error: true,
            },
            &mut st,
        );
        // A tool error must not mark the whole turn as failed.
        assert!(!st.had_error);
        let r = handle_ui_event(UiEvent::TurnComplete, &mut st);
        assert_eq!(r, Some(EXIT_SUCCESS));
        // ... but it does grade the reward down, without changing the verdict.
        let reward = st.outcome.conclude(conclusion_for(EXIT_SUCCESS));
        assert!(
            reward.reward > 0.0 && reward.reward < 1.0,
            "graded reward expected, got {}",
            reward.reward
        );
        assert_eq!(reward.outcome, "success_with_tool_errors");
    }

    #[test]
    fn conclusion_for_maps_every_exit_code() {
        assert_eq!(conclusion_for(EXIT_SUCCESS), RunConclusion::Success);
        assert_eq!(conclusion_for(EXIT_TIMEOUT), RunConclusion::Timeout);
        assert_eq!(
            conclusion_for(EXIT_BUDGET_EXHAUSTED),
            RunConclusion::BudgetExhausted
        );
        assert_eq!(conclusion_for(EXIT_AGENT_ERROR), RunConclusion::AgentError);
        // Anything unexpected is a failure, never a silent success.
        assert_eq!(conclusion_for(42), RunConclusion::AgentError);
    }

    #[test]
    fn an_aborted_run_exits_zero_but_scores_zero() {
        // `Aborted` returns EXIT_SUCCESS (a cancelled run is not a CI failure),
        // so the event stream is the only thing that knows not to train on it.
        let mut st = state(0);
        let r = handle_ui_event(
            UiEvent::Aborted {
                partial_text: "half a thought".into(),
            },
            &mut st,
        );
        assert_eq!(r, Some(EXIT_SUCCESS));
        let reward = st.outcome.conclude(conclusion_for(EXIT_SUCCESS));
        assert_eq!(reward.reward, 0.0);
        assert_eq!(reward.outcome, "cancelled");
    }

    #[test]
    fn the_conversation_is_recorded_into_the_trajectory() {
        let mut st = state(0);
        st.assembler.push_message(&Message::user("do a thing"));
        handle_ui_event(
            UiEvent::ToolCallStarted(ToolCall {
                id: "tc-1".into(),
                name: "read_file".into(),
                args: serde_json::json!({"path": "a.txt"}),
            }),
            &mut st,
        );
        handle_ui_event(
            UiEvent::ToolCallFinished {
                call_id: "tc-1".into(),
                tool_name: "read_file".into(),
                output: "contents".into(),
                is_error: false,
            },
            &mut st,
        );
        handle_ui_event(UiEvent::TextComplete("all done".into()), &mut st);

        let steps = st.assembler.finish();
        assert!(!steps.is_empty(), "the run must produce trace steps");
        let rendered = serde_json::to_string(&steps).expect("serialize steps");
        assert!(rendered.contains("do a thing"), "user prompt missing");
        assert!(rendered.contains("read_file"), "tool call missing");
        assert!(rendered.contains("contents"), "tool result missing");
        assert!(rendered.contains("all done"), "assistant reply missing");
    }
}
