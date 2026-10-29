// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Kernel-based CI runner that drives a single session to completion.
//!
//! This is the HSM-era replacement for [`super::CiRunner`]. It:
//!
//! 1. Opens a kernel session through the frontend's [`SessionController`]
//!    in the reactive `agent` mode (the streaming, native-tool-calling coding
//!    agent), answering every human gate at once - nobody is there to ask
//!    in CI.
//! 2. Receives the session's [`AgentEvent`] stream.
//! 3. Posts the initial [`Event::UserMessage`] from the step prompt.
//! 4. Bridges every [`UiEvent`] to CI output: assistant text → stdout,
//!    diagnostics (thinking, tool progress, usage, errors) → stderr.
//! 5. Returns exit code 0 when the turn completes, non-zero on error/timeout.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Context as _;
use sven_bootstrap::Config;
use sven_bootstrap::RuntimeContext;
use sven_bootstrap::TimeBudget;
use sven_frontend::{SessionController, SessionOptions, SessionSpec};
use sven_hsm::UiEvent;
use sven_model::Message;
use sven_session_store::trace_session::StepAssembler;
use sven_vocab::AgentMode;

use crate::call_records::CallRecords;
use crate::ledger_output::LedgerOutput;
use crate::output::{finalise_stdout, write_progress, write_stderr};
use crate::runner::outcome::{exit_code_for_verified_task, verdict_from_context};
use crate::runner::ui_output::{
    close_sven_section, handle_ui_event, print_total_usage, write_trajectory, CiOutState,
};
use crate::runner::{EXIT_AGENT_ERROR, EXIT_SUCCESS, EXIT_TIMEOUT};

// ── RuntimeRunner ─────────────────────────────────────────────────────────────

/// Headless CI runner backed by the HSM kernel.
///
/// Runs exactly one prompt and waits for the reactive agent turn to finish,
/// answering every human gate at once. Multi-step workflow / JSONL
/// piping continues to live in [`super::CiRunner`]; this runner is the
/// single-prompt kernel path.
pub struct RuntimeRunner {
    config: Arc<Config>,
}

/// Options for a single kernel-driven CI run.
#[derive(Debug)]
pub struct RuntimeRunnerOptions {
    /// Mode string (e.g. `"agent"`, `"chat"`, `"sdlc"`) - selects which kernel
    /// machine runs. `Self::kernel_mode` collapses `Plan`/`Research`/`Agent`
    /// to the same "agent" machine string; `agent_mode` below is what
    /// actually differentiates their PERMISSION policy.
    pub mode: String,
    /// The original interactive [`AgentMode`], the permissions the session
    /// runs under. Without this, `--mode plan`/`--mode research` would run
    /// with full write permissions -- see the identical field on
    /// `crate::kernel_agent::KernelAgent`.
    pub agent_mode: AgentMode,
    /// The single user prompt to execute.
    pub prompt: String,
    /// Prior conversation history to seed into the kernel thread before the
    /// prompt is posted. Used to replay a piped prior-conversation document
    /// (`sven '...' | sven --stdin 'next task'`) as context. Empty for a fresh run.
    pub history: Vec<Message>,
    /// Project root (used for checkpoint path, audit log, etc.).
    pub project_root: Option<std::path::PathBuf>,
    /// Per-run timeout in seconds (`0` = unlimited). When unset, the per-step
    /// timeout and then `agent.max_run_timeout_secs` bound the run.
    pub timeout_secs: Option<u64>,
    /// Per-step timeout in seconds (`0` = unlimited); for a single-turn kernel
    /// run, the fallback bound when no per-run timeout is supplied.
    pub step_timeout_secs: Option<u64>,
    /// Cumulative token budget for the run. When the running total of input +
    /// output tokens reaches this value the run stops with
    /// [`EXIT_BUDGET_EXHAUSTED`](crate::runner::EXIT_BUDGET_EXHAUSTED). `None` means unlimited (mirrors the legacy
    /// runner's `--max-tokens`).
    pub max_tokens_budget: Option<u64>,
    /// Extra text appended to the composed system prompt (mirrors the legacy
    /// runner's `--append-system-prompt`). `None` leaves the prompt unchanged.
    pub append_system_prompt: Option<String>,
    /// Suppress Sven's built-in system prompt (`--no-system`). See
    /// `sven_turn::AgentRuntimeContext::build_system_message` for exact
    /// semantics when combined with `append_system_prompt`.
    pub no_system: bool,
    /// Disable all tools for this session (`--no-tools`): no tool schemas are
    /// sent to the model and any tool call is refused.
    pub no_tools: bool,
    /// Verbosity level (0 = minimal, 1 = verbose, 2+ = trace).
    pub trace_level: u8,
    /// Print one `[sven:ledger]` JSON line to stderr per model request
    /// (`--token-ledger`). The ledger is recorded in the trajectory either way.
    pub token_ledger: bool,
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
            "verified-task" => "verified-task",
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
        let project_context_file = if opts.no_system {
            None
        } else {
            opts.project_root
                .as_ref()
                .and_then(|r| sven_workspace::find_project_context_file(r))
        };
        if let Some(path) = &project_context_file {
            write_stderr(&format!(
                "[sven:info] Project context file found at {} (referenced, not auto-loaded)",
                path.display()
            ));
        }

        // The deadline is enforced per `recv` rather than by racing the whole
        // drive loop in a `select!`: dropping that future on timeout would
        // take `state` - and with it the trajectory accumulated so far - down
        // with it, leaving a timed-out run with no record at all. It is fixed
        // before the session is built so that the model is told the same
        // budget the run is held to.
        let started = Instant::now();
        let deadline = super::deadline::run_deadline(
            started,
            opts.timeout_secs.or(opts.step_timeout_secs),
            self.config.agent.max_run_timeout_secs,
        );

        let runtime_ctx = RuntimeContext {
            project_context_file,
            append_system_prompt: opts.append_system_prompt.clone(),
            system_prompt_override: self.config.agent.system_prompt.clone(),
            no_system: opts.no_system,
            no_tools: opts.no_tools,
            time_budget: deadline.map(|by| TimeBudget::starting_now(by - started)),
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

        // Nobody is at a headless run: its gates are answered at once, and an
        // MCP server that wants a browser sign-in is not given one.
        let (events, mut events_rx) = tokio::sync::mpsc::channel(256);
        let mut options = SessionOptions::new(Arc::clone(&self.config), runtime_ctx, events);
        options.allow_interactive_oauth = false;
        let spec = SessionSpec {
            machine: kernel_mode.to_string(),
            permissions: opts.agent_mode,
            model: self.config.model.clone(),
        };
        let (session, _mcp_events) = SessionController::open(options, spec, history)
            .await
            .context("failed to build kernel session")?;

        // Post the user prompt.
        session
            .post(opts.prompt.clone())
            .await
            .map_err(|e| anyhow::anyhow!("{e} before UserMessage was delivered"))?;

        // The trajectory this run records: history replayed into the kernel
        // first, then the new prompt, so the document stands on its own.
        let mut assembler = StepAssembler::new();
        for msg in &opts.history {
            assembler.push_message(msg);
        }
        assembler.push_message(&Message::user(&opts.prompt));

        let mut state = CiOutState::new(
            opts.trace_level,
            opts.prompt.clone(),
            opts.max_tokens_budget,
            assembler,
            LedgerOutput::new(opts.token_ledger),
        );

        // The step banner goes to stderr immediately; the `## User`
        // conversation section on stdout is written lazily (see
        // `ensure_user_header`) - only once the turn is about to
        // produce its first real stdout content. A turn that fails
        // before producing anything must leave stdout untouched
        // (`11_error_handling.bats`'s "produces no stdout content"),
        // and eagerly writing "## User" here (as this used to)
        // unconditionally broke that contract on every provider error.
        write_progress("[sven:step:start] 1/1");

        // `TurnExecutor` emits `UiEvent::TurnComplete` whenever a turn
        // produces neither tool calls nor an empty response - a heuristic
        // that means "hand control back" for chat/agent/sdlc, but is a false
        // positive for verified-task: a tool-free final turn there only
        // means the model *claims* to be done, and `Attempting` reacts by
        // emitting `Effect::Verify`, not by ending the run. Treating that
        // first claim as `TurnComplete` would exit before verification (and
        // any retry) ever ran. So for this mode alone, that signal is
        // swallowed instead.
        //
        // That leaves the real question open: what *does* signal completion?
        // Not the event stream closing - the session (which this function
        // still owns at this point) keeps the kernel's observation sink alive
        // for its entire lifetime, including through the `join()` call below,
        // so the stream never ends while this function is still running:
        // waiting for it to close here would deadlock forever.
        // The kernel's status watch has no such problem - `status_rx` is an
        // independent `watch::Receiver` - so for this mode alone the loop
        // also races it and treats `RuntimeStatus.done` (set exactly when
        // `VerifiedTaskMachine::is_terminal` does) as the real end. `result`
        // from that branch is only a placeholder; the actual exit code is
        // computed from the joined `Context` after the loop (see below).
        let is_verified_task = kernel_mode == "verified-task";
        let mut status_rx = session.handle().status_watch();

        let result = loop {
            if is_verified_task && status_rx.borrow().done {
                break EXIT_SUCCESS;
            }
            let received = match deadline {
                Some(dl) => {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    tokio::select! {
                        r = tokio::time::timeout(remaining, events_rx.recv()) => match r {
                            Ok(r) => r,
                            Err(_) => {
                                write_stderr(&format!(
                                    "[sven:error] RuntimeRunner timed out after {}s",
                                    (dl - started).as_secs()
                                ));
                                break EXIT_TIMEOUT;
                            }
                        },
                        _ = status_rx.wait_for(|s| s.done), if is_verified_task => {
                            break EXIT_SUCCESS;
                        }
                    }
                }
                None => {
                    tokio::select! {
                        r = events_rx.recv() => r,
                        _ = status_rx.wait_for(|s| s.done), if is_verified_task => {
                            break EXIT_SUCCESS;
                        }
                    }
                }
            };
            match received {
                Some(ev) => {
                    let suppress_turn_complete =
                        is_verified_task && matches!(ev, UiEvent::TurnComplete);
                    if let Some(done) = handle_ui_event(ev, &mut state) {
                        if !suppress_turn_complete {
                            break done;
                        }
                    }
                }
                // The session ended and dropped the sender; treat as done.
                // Close any open `## Sven` streaming section first so the
                // trailing conversation document is well-formed for a
                // downstream pipe stage (mirrors the TurnComplete / Aborted
                // finalisation paths).
                None => {
                    close_sven_section(&mut state);
                    finalise_stdout(&state.streamed_text);
                    print_total_usage(&state);
                    break if state.had_error {
                        EXIT_AGENT_ERROR
                    } else {
                        EXIT_SUCCESS
                    };
                }
            }
        };

        // What the session recorded about its calls, read before it is
        // detached or joined away.
        let call_records = CallRecords::of(&session.handle());

        // The verified-task machine's `Done`/`Parked` states are terminal
        // (unlike chat/agent/sdlc, which idle forever awaiting the next
        // prompt), so its kernel task actually returns once the run
        // concludes and `join` is safe - it is also the only way to read the
        // verdict the machine recorded in its final `Context`, since neither
        // `RuntimeStatus` nor the UI-event stream carries facts. `result`
        // from the loop above is only a placeholder for this mode (the
        // observation stream carries no exit-code-worthy signal once
        // `TurnComplete` is suppressed - see the loop's comment); the real
        // exit code is derived from the joined context below. Every other
        // mode keeps the existing detach-and-forget behaviour and its
        // loop-derived `result` unchanged.
        let (result, verdict) = if is_verified_task {
            match session.join().await {
                Ok(report) => (
                    exit_code_for_verified_task(&report.ctx),
                    verdict_from_context(&report.ctx),
                ),
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:warn] verified-task kernel task did not join cleanly: {e}"
                    ));
                    (EXIT_AGENT_ERROR, None)
                }
            }
        } else {
            // Keep the runtime alive until here so the audit log flushes.
            session.detach();
            (result, None)
        };

        write_progress(&format!(
            "[sven:step:complete] 1/1 duration_ms={} tools={} success={}",
            started.elapsed().as_millis(),
            state.tools_used,
            result == EXIT_SUCCESS
        ));

        call_records.feed(&mut state.assembler);
        write_trajectory(state, result, &self.config, &opts.mode, verdict);

        Ok(result)
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
            no_tools: false,
            trace_level: 0,
            token_ledger: false,
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
        assert_eq!(RuntimeRunner::kernel_mode("verified-task"), "verified-task");
    }
}
