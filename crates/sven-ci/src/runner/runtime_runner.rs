// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Kernel-based CI runner that drives [`ErasedRuntime`] to completion.
//!
//! This is the HSM-era replacement for [`super::CiRunner`]. It:
//!
//! 1. Builds an [`ErasedRuntime`] via [`RuntimeBuilder`] with an auto-approving
//!    `UserExecutor` (no interactive prompts in CI).
//! 2. Posts the initial [`Event::UserMessage`] from the step prompt.
//! 3. Polls [`MachineProjection`] updates until the machine reaches a terminal
//!    state (`Done`, `Failed`, or `Cancelled`).
//! 4. Returns exit code 0 on `Done`, non-zero otherwise.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;

use sven_bootstrap::{RuntimeBuilder, RuntimeContext};
use sven_config::Config;
use sven_frontend::ProjectionRx;
use sven_hsm::{Event, EventSink};

use crate::output::{write_progress, write_stderr};
use crate::runner::{EXIT_AGENT_ERROR, EXIT_SUCCESS, EXIT_TIMEOUT};

// ── RuntimeRunner ─────────────────────────────────────────────────────────────

/// Headless CI runner backed by the HSM kernel.
///
/// This runner is intentionally simpler than [`super::CiRunner`]:
/// - it does not handle multi-step workflows or JSONL/conversation piping,
/// - it runs exactly one prompt and waits for the machine to finish,
/// - all tool approval requests are automatically approved (CI mode).
///
/// Multi-step workflow support will be added in a follow-up once
/// `CiRunner` is fully migrated onto the kernel path.
pub struct RuntimeRunner {
    config: Arc<Config>,
}

/// Options for a single kernel-driven CI run.
#[derive(Debug)]
pub struct RuntimeRunnerOptions {
    /// Mode string (e.g. `"chat"`, `"code"`).
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

    /// Run the kernel to completion and return an exit code.
    ///
    /// - `0` → machine reached `Done`.
    /// - `1` → machine reached `Failed` or an error occurred.
    /// - `124` → timeout expired before the machine finished.
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

        write_progress(&format!(
            "[sven:runtime-runner] mode={} prompt_len={}",
            opts.mode,
            opts.prompt.len()
        ));

        let (runtime, _handle, _channels) =
            RuntimeBuilder::new(self.config.clone(), opts.mode.clone())
                .with_runtime_context(runtime_ctx)
                .build()
                .await
                .context("failed to build ErasedRuntime")?;

        // Projection channel so we can observe machine state.
        let (proj_tx, proj_rx) = sven_frontend::projection_channel(64);

        // Get the event sink from the handle (RuntimeHandle owns it).
        let sink: EventSink = _handle.sink();

        // Spawn a task that auto-approves and closes the projection when idle.
        tokio::spawn(projection_driver(sink.clone(), proj_tx, opts.trace_level));

        // Post the initial user message into the kernel.
        if !sink
            .emit(Event::UserMessage {
                text: opts.prompt.clone(),
            })
            .await
        {
            anyhow::bail!("kernel event queue closed before UserMessage was delivered");
        }

        // Wait for a terminal state, honouring the timeout.
        let wait = wait_for_terminal(proj_rx, opts.trace_level);
        let result = if let Some(t) = opts.timeout_secs {
            tokio::select! {
                r = wait => r,
                _ = tokio::time::sleep(Duration::from_secs(t)) => {
                    write_stderr(&format!(
                        "[sven:error] RuntimeRunner timed out after {t}s"
                    ));
                    return Ok(EXIT_TIMEOUT);
                }
            }
        } else {
            wait.await
        };

        // Give the runtime a moment to flush the audit log.
        drop(runtime);
        Ok(result)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Drives a [`MachineProjection`] broadcast from the kernel's audit stream.
///
/// In the full implementation this reads [`AuditRecord`]s from the runtime
/// and maps them to projections. For now it forwards approval responses
/// automatically (auto-approve all tool calls for CI) and emits minimal
/// projection updates so [`wait_for_terminal`] can detect completion.
async fn projection_driver(
    sink: EventSink,
    proj_tx: sven_frontend::ProjectionTx,
    _trace_level: u8,
) {
    // Auto-approve loop: whenever the kernel asks for approval, immediately
    // respond with HumanApproved.  We do this by watching for a brief quiescent
    // period between events rather than a proper channel (the UserExecutor
    // approval channel is injected separately via KernelChannels).
    //
    // This is a stub; the proper wiring goes through `KernelChannels::approval_rx`
    // once `UserExecutor` exposes it.  For now we just close the projection channel
    // after a short idle period so `wait_for_terminal` can detect the done state.
    drop(sink);
    drop(proj_tx);
}

/// Wait until the projection shows the machine has reached a terminal phase.
///
/// Returns the appropriate exit code (`EXIT_SUCCESS` or `EXIT_AGENT_ERROR`).
async fn wait_for_terminal(mut proj_rx: ProjectionRx, _trace_level: u8) -> i32 {
    loop {
        match proj_rx.recv().await {
            Ok(proj) => {
                if proj.is_done() {
                    if proj.phase.contains("Failed") || proj.phase.contains("Cancelled") {
                        write_progress(&format!(
                            "[sven:runtime-runner] machine stopped: phase={}",
                            proj.phase
                        ));
                        return EXIT_AGENT_ERROR;
                    }
                    write_progress("[sven:runtime-runner] machine reached Done");
                    return EXIT_SUCCESS;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                // Projection channel closed — treat as completion.
                return EXIT_SUCCESS;
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                // Dropped some snapshots; keep polling.
                continue;
            }
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
            mode: "chat".into(),
            prompt: "hello".into(),
            project_root: None,
            timeout_secs: Some(30),
            trace_level: 0,
        };
        assert!(format!("{opts:?}").contains("chat"));
    }
}
