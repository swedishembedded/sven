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
use std::time::Duration;

use anyhow::Context as _;
use tokio::sync::broadcast::error::RecvError;

use sven_bootstrap::{KernelChannels, RuntimeBuilder, RuntimeContext};
use sven_config::Config;
use sven_hsm::{Event, EventSink, UiEvent};

use crate::output::{
    finalise_stdout, format_token_usage_line, write_progress, write_stderr, write_stdout,
};
use crate::runner::{EXIT_AGENT_ERROR, EXIT_SUCCESS, EXIT_TIMEOUT};

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
    /// Mode string (e.g. `"agent"`, `"chat"`, `"sdlc"`).
    pub mode: String,
    /// The single user prompt to execute.
    pub prompt: String,
    /// Project root (used for checkpoint path, audit log, etc.).
    pub project_root: Option<std::path::PathBuf>,
    /// Per-run timeout in seconds. `None` means unlimited.
    pub timeout_secs: Option<u64>,
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
        let runtime_ctx = RuntimeContext {
            project_root: opts.project_root.clone(),
            git_context: opts
                .project_root
                .as_ref()
                .map(|r| sven_runtime::collect_git_context(r)),
            ci_context: Some(crate::context::detect_ci_context()),
            project_context_file: None,
            append_system_prompt: None,
            system_prompt_override: None,
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

        let bundle = RuntimeBuilder::new(self.config.clone(), kernel_mode)
            .with_runtime_context(runtime_ctx)
            .with_allow_interactive_oauth(false)
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
        let drive = async {
            let mut streamed_text = String::new();
            let mut had_error = false;
            loop {
                match obs_rx.recv().await {
                    Ok(ev) => {
                        if let Some(done) =
                            handle_ui_event(ev, trace, &mut streamed_text, &mut had_error)
                        {
                            return done;
                        }
                    }
                    // The session ended and dropped the sender; treat as done.
                    Err(RecvError::Closed) => break,
                    // Dropped some observations under load; keep going.
                    Err(RecvError::Lagged(_)) => continue,
                }
            }
            finalise_stdout(&streamed_text);
            if had_error {
                EXIT_AGENT_ERROR
            } else {
                EXIT_SUCCESS
            }
        };

        let result = if let Some(t) = opts.timeout_secs {
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

/// Bridge a single [`UiEvent`] to CI output.
///
/// Returns `Some(exit_code)` when the turn has settled (the caller should stop
/// driving the loop), or `None` to keep going.
fn handle_ui_event(
    ev: UiEvent,
    trace: u8,
    streamed_text: &mut String,
    had_error: &mut bool,
) -> Option<i32> {
    match ev {
        UiEvent::TextDelta(d) => {
            streamed_text.push_str(&d);
            write_stdout(&d);
        }
        // Deltas were already streamed; only emit the complete text if nothing
        // streamed (some providers send a single TextComplete with no deltas).
        UiEvent::TextComplete(t) => {
            if streamed_text.is_empty() && !t.is_empty() {
                streamed_text.push_str(&t);
                write_stdout(&t);
            }
        }
        UiEvent::ThinkingDelta(_) => {}
        UiEvent::ThinkingComplete(c) => {
            if trace >= 2 && !c.is_empty() {
                write_progress(&format!("[sven:thinking] {c}"));
            }
        }
        UiEvent::ToolStarted { name, call_id, .. } => {
            write_progress(&format!("[sven:tool:start] name={name} id={call_id}"));
        }
        UiEvent::ToolProgress { call_id, message } => {
            if trace >= 1 {
                write_progress(&format!("[sven:tool:progress] id={call_id} {message}"));
            }
        }
        UiEvent::ToolFinished {
            name,
            is_error,
            output,
            ..
        } => {
            let status = if is_error { "error" } else { "ok" };
            write_progress(&format!("[sven:tool:done] name={name} status={status}"));
            if is_error {
                write_stderr(&format!("[sven:tool:error] {name}: {output}"));
            }
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
            if trace >= 1 {
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
            if trace >= 2 {
                write_progress(&format!("[sven:transition] {from} -> {to} on {event}"));
            }
        }
        UiEvent::Error(e) => {
            *had_error = true;
            write_stderr(&format!("[sven:error] {e}"));
        }
        UiEvent::TurnComplete => {
            finalise_stdout(streamed_text);
            return Some(if *had_error {
                EXIT_AGENT_ERROR
            } else {
                EXIT_SUCCESS
            });
        }
        UiEvent::Aborted { partial_text } => {
            if streamed_text.is_empty() && !partial_text.is_empty() {
                write_stdout(&partial_text);
            }
            finalise_stdout(streamed_text);
            return Some(EXIT_SUCCESS);
        }
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
            prompt: "hello".into(),
            project_root: None,
            timeout_secs: Some(30),
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

    #[test]
    fn text_delta_streams_to_buffer() {
        let mut buf = String::new();
        let mut err = false;
        let r = handle_ui_event(UiEvent::TextDelta("pong".into()), 0, &mut buf, &mut err);
        assert!(r.is_none());
        assert_eq!(buf, "pong");
        assert!(!err);
    }

    #[test]
    fn turn_complete_returns_success() {
        let mut buf = String::from("pong");
        let mut err = false;
        let r = handle_ui_event(UiEvent::TurnComplete, 0, &mut buf, &mut err);
        assert_eq!(r, Some(EXIT_SUCCESS));
    }

    #[test]
    fn error_then_turn_complete_returns_agent_error() {
        let mut buf = String::new();
        let mut err = false;
        handle_ui_event(UiEvent::Error("boom".into()), 0, &mut buf, &mut err);
        let r = handle_ui_event(UiEvent::TurnComplete, 0, &mut buf, &mut err);
        assert_eq!(r, Some(EXIT_AGENT_ERROR));
    }
}
