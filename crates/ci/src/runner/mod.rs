// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0

mod event;
mod helpers;
pub mod runtime_runner;

use event::{handle_event, stream_new_steps, StepState, SubagentChildState};
pub(crate) use helpers::{
    is_conversation_format, is_json_summary_format, is_jsonl_format, parse_json_summary,
    parse_jsonl_trace_steps,
};
use helpers::{
    normalize_label, parse_agent_mode, sanitize_cache_key, write_conversation_artifact,
    write_step_artifact,
};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::mpsc;

use atif::{TraceStep, Trajectory};
use sven_bootstrap::RuntimeContext;
use sven_config::{AgentMode, Config};
use sven_machines::AgentEvent;
use sven_model::{ContentPart, Message, MessageContent, Role};
use sven_session_store::trace_session::{self, StepAssembler, SvenSessionMeta};
use sven_session_store::{
    apply_outcome_to_trajectory, parse_conversation, parse_frontmatter, parse_workflow,
    OutcomeFold, RunConclusion, SessionOutcome, Step, StepQueue,
};
use sven_workspace::resolve_auto_log_path;

use crate::kernel_agent::KernelAgent;

use crate::output::{write_progress, write_stderr, write_stdout};
use crate::template::apply_template;

// ── Exit codes ────────────────────────────────────────────────────────────────

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_AGENT_ERROR: i32 = 1;
pub const EXIT_VALIDATION_ERROR: i32 = 2;
/// One or more tool calls returned errors during the run but the run completed.
/// Allows `set -e` pipelines to treat partial failures differently from hard errors.
pub const EXIT_TOOL_WARNINGS: i32 = 3;
/// The token budget set via `--max-tokens` was exhausted before all steps completed.
pub const EXIT_BUDGET_EXHAUSTED: i32 = 4;
/// A tool call parked awaiting a human answer that did not arrive during this
/// run (see `Event::QuestionAsked`). Not a failure: the trajectory is left
/// unscored (no reward stamped) rather than concluded, so it can be resumed
/// once a human answers - see `sven questions`.
pub const EXIT_NEEDS_HUMAN: i32 = 5;
pub const EXIT_TIMEOUT: i32 = 124;
pub const EXIT_INTERRUPT: i32 = 130;

// ── Output format ─────────────────────────────────────────────────────────────

/// Controls what sven writes to stdout for each headless run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Full conversation format: `## User` / `## Sven` / `## Tool` / `## Tool Result`.
    /// Output is valid sven conversation markdown that can be piped back into
    /// another sven instance or loaded with `--conversation`.
    #[default]
    Conversation,
    /// Structured JSON: the run's full ATIF `Trajectory` document (schema
    /// version, agent profile, and every accumulated `steps` entry),
    /// pretty-printed to stdout at the end of the run.  Not designed for
    /// piping between sven instances the way `Jsonl` is (it's one big object,
    /// not one-record-per-line); use `--output-format jsonl` for that, or
    /// `--output-trace`/`--trace` to write the same document to a file.
    Json,
    /// Compact plain text: only the final agent response for each step,
    /// without section headings.  Matches the legacy pre-1.0 behaviour.
    Compact,
    /// Full-fidelity JSONL: one ATIF `TraceStep` JSON object per line,
    /// streamed to stdout in real-time as each step is known complete.
    /// Designed for piping:
    ///
    ///   sven 'task 1' --output-format jsonl | sven 'task 2'
    ///
    /// The receiving sven instance detects the JSONL format automatically via
    /// `is_jsonl_format()` and loads it as prior conversation history.  See
    /// `--output-trace`/`--trace` to persist the equivalent full trajectory
    /// document to a file instead of streaming step-by-step.
    Jsonl,
}

// ── Options ───────────────────────────────────────────────────────────────────

/// Options for the CI runner.
#[derive(Debug)]
pub struct CiOptions {
    pub mode: AgentMode,
    pub model_override: Option<String>,
    /// The raw markdown input to process.  May come from a file or stdin.
    pub input: String,
    /// Extra prompt prepended before the first step (from positional CLI args)
    pub extra_prompt: Option<String>,
    /// When true, input was read from a workflow file (`-f`/`--file`); workflow
    /// parsing (## steps, preamble, frontmatter) applies.  When false, input
    /// is from stdin or empty - no workflow parsing, plain/text is one step.
    pub input_from_file: bool,
    /// Absolute path to the project root (auto-detected from `.git`).
    pub project_root: Option<PathBuf>,
    /// Output format for stdout.
    pub output_format: OutputFormat,
    /// Directory to write per-run artifacts to (optional).
    pub artifacts_dir: Option<PathBuf>,
    /// Template variables substituted as `{{key}}` in step content.
    pub vars: HashMap<String, String>,
    /// Per-step timeout override from CLI (seconds; 0 = no limit).
    pub step_timeout_secs: Option<u64>,
    /// Total run timeout override from CLI (seconds; 0 = no limit).
    pub run_timeout_secs: Option<u64>,
    /// Dry-run: parse and validate workflow, then exit without calling the model.
    pub dry_run: bool,
    /// Write the final agent response text to this file after the run.
    pub output_last_message: Option<PathBuf>,
    /// Override the system prompt by reading from this file path.
    pub system_prompt_file: Option<PathBuf>,
    /// Text appended to the default system prompt (after Guidelines section).
    pub append_system_prompt: Option<String>,
    /// Suppress Sven's built-in system prompt (`--no-system`). See
    /// [`sven_machines::AgentRuntimeContext::build_system_message`] for exact
    /// semantics when combined with `system_prompt_file`/`append_system_prompt`.
    pub no_system: bool,
    /// Disable all tools for this session (`--no-tools`): no tool schemas are
    /// sent to the model and any tool call is refused.
    pub no_tools: bool,
    /// Stderr trace verbosity (mirrors CLI --verbose count).
    /// 0 = minimal (default): tool name, success/fail, size.
    /// 1 = verbose (-v): include truncated tool output and thinking blocks.
    /// 2+ = trace (-vv): reserved for future expanded tracing.
    pub trace_level: u8,
    /// Load conversation history from this ATIF trajectory file
    /// (`--load-trace`/`--trace`) before running.  History seeds the agent;
    /// workflow steps run on top of it.
    pub load_trace: Option<PathBuf>,
    /// Write the full ATIF trajectory to this path after every step.
    /// If `None`, falls back to the auto-log path
    /// (`.sven/logs/<timestamp>.atif.json`).
    pub output_trace: Option<PathBuf>,
    /// Replay all tool calls recorded in the loaded trajectory with fresh
    /// results before submitting to the model.  Only meaningful when
    /// `load_trace` is set.
    pub rerun_toolcalls: bool,
    /// When loading a trajectory, regenerate the system prompt from current
    /// skills and config instead of reusing a stored one.
    ///
    /// Note: ATIF trajectories never persist the system prompt as a step (see
    /// `sven_session_store::trace_session::StepAssembler::push_message`'s System-role
    /// handling) — the agent always regenerates it. This flag therefore
    /// currently has no additional effect on trace-loaded runs; it is kept
    /// for CLI compatibility and in case a future ATIF `extra` convention
    /// restores stored-system-prompt reuse.
    pub regen_system_prompt: bool,
    /// Maximum total tokens (input + output) across the entire run.
    /// When this budget is exhausted the runner exits with [`EXIT_BUDGET_EXHAUSTED`] (4).
    /// `None` or `0` means unlimited.
    pub max_tokens_budget: Option<u64>,
    /// Files attached to the **initial** user turn (from repeated `--attach`).
    ///
    /// Unlike the `attach_file` tool this needs no tool call, so it works with
    /// models that have no tool-calling support at all.
    pub attachments: Vec<PathBuf>,
}

// ── Attachment loading (`--attach`) ───────────────────────────────────────────

/// Build the initial user turn's content parts from the prompt plus `paths`.
///
/// Classification and loading go through `sven_tools_fs::load_attachment`, the
/// same function the `attach_file` tool uses, so the CLI flag and the tool can
/// never disagree about how a path becomes a content part.
async fn build_attachment_parts(
    prompt: &str,
    paths: &[PathBuf],
    model: &Arc<dyn sven_model::ModelProvider>,
    asr: &sven_config::AsrConfig,
) -> anyhow::Result<Vec<sven_model::ContentPart>> {
    let opts = sven_tools_fs::AttachOptions {
        supports_images: model.supports_images(),
        supports_audio: model.supports_audio(),
        force_transcribe: false,
        asr: asr.clone(),
        // Production dials its own client from `asr`; only tests inject one.
        asr_client: None,
    };
    let label = format!("{}/{}", model.name(), model.model_name());

    let mut parts = vec![sven_model::ContentPart::text(prompt)];
    for path in paths {
        let loaded = sven_tools_fs::load_attachment(path, &opts, &label)
            .await
            .with_context(|| format!("attaching {}", path.display()))?;
        write_stderr(&format!(
            "[sven:attach] {}",
            loaded.text().lines().next().unwrap_or("")
        ));
        parts.extend(loaded.into_content_parts());
    }

    // If every attachment resolved to text (e.g. all audio was transcribed),
    // merge into one text part.  `Message::user_with_parts` then collapses it
    // to a plain string message, so the turn is indistinguishable from an
    // ordinary prompt for every provider.
    if parts
        .iter()
        .all(|p| matches!(p, sven_model::ContentPart::Text { .. }))
    {
        let merged = parts
            .iter()
            .filter_map(|p| match p {
                sven_model::ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(vec![sven_model::ContentPart::text(merged)]);
    }

    Ok(parts)
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Headless CI runner that processes a [`StepQueue`] sequentially.
pub struct CiRunner {
    config: Arc<Config>,
}

impl CiRunner {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub async fn run(&self, opts: CiOptions) -> anyhow::Result<()> {
        // ── Parse frontmatter ────────────────────────────────────────────────
        let (frontmatter, markdown_body) = parse_frontmatter(&opts.input);
        let frontmatter = frontmatter.unwrap_or_default();

        // ── Merge template vars (CI env < workspace < frontmatter < CLI) ───────
        // CI environment variables are injected at the lowest priority so
        // workflows can reference {{branch}}, {{commit}}, {{GITHUB_SHA}}, etc.
        // without any explicit --var flag.
        let ci_ctx = crate::context::detect_ci_context();
        let mut vars: HashMap<String, String> = crate::context::ci_template_vars(&ci_ctx);

        // Inject built-in workspace path vars so workflows can reference
        // {{PROJECT_ROOT}} and {{WORKSPACE_ROOT}} without needing --var flags.
        // WORKSPACE_ROOT is the nearest ancestor of the project root that
        // contains a recognised workspace marker (see find_workspace_root).
        if let Some(ref root) = opts.project_root {
            vars.entry("PROJECT_ROOT".into())
                .or_insert_with(|| root.to_string_lossy().into_owned());
            let ws_root = crate::context::find_workspace_root(root);
            vars.entry("WORKSPACE_ROOT".into())
                .or_insert_with(|| ws_root.to_string_lossy().into_owned());
        }

        vars.extend(frontmatter.vars.unwrap_or_default());
        vars.extend(opts.vars.clone());

        // ── Detect piped input format ─────────────────────────────────────────
        // Priority: JSONL > conversation markdown > JSON summary > workflow / plain-text.
        //
        // JSONL: produced by `--output-format jsonl`.
        //   Every non-empty line is a standalone ATIF `TraceStep` JSON object.
        //   Carries full-fidelity history including thinking blocks and tool
        //   calls (folded into turn-shaped steps).
        //
        // Conversation markdown: default output of `--output-format conversation`.
        //   Contains `## User` / `## Sven` / `## Tool` / `## Tool Result`
        //   headings.  Must NOT be treated as a workflow (those headings would
        //   be misread as step labels).
        //
        // JSON summary: produced by `--output-format json`.
        //   A single pretty-printed ATIF `Trajectory` document.  Reconstructed
        //   into a simple message history for the receiving instance.
        //
        // Both JSONL and conversation formats may end with a trailing user turn
        // that has not yet received a response.  When present, that pending turn
        // is used as the step content when no CLI positional prompt was supplied.
        let is_jsonl_input = !opts.input.trim().is_empty() && is_jsonl_format(markdown_body);

        let is_conversation_input = !is_jsonl_input
            && !opts.input.trim().is_empty()
            && is_conversation_format(markdown_body);

        let is_json_summary_input = !is_jsonl_input
            && !is_conversation_input
            && !opts.input.trim().is_empty()
            && is_json_summary_format(markdown_body);

        // Parse the piped input: extract history to seed the agent and any
        // trailing pending user turn that was not yet answered.
        let (conversation_history, piped_pending) = if is_jsonl_input {
            match parse_jsonl_trace_steps(markdown_body) {
                Ok((history, pending)) => (history, pending),
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:warn] Failed to parse piped input as JSONL trace steps ({e}), \
                         treating as workflow"
                    ));
                    (Vec::new(), None)
                }
            }
        } else if is_conversation_input {
            match parse_conversation(markdown_body) {
                Ok(conv) => (conv.history, conv.pending_user_input),
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:warn] Failed to parse piped input as conversation ({e}), \
                         treating as workflow"
                    ));
                    (Vec::new(), None)
                }
            }
        } else if is_json_summary_input {
            match parse_json_summary(markdown_body) {
                Ok(history) => (history, None),
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:warn] Failed to parse piped input as JSON summary ({e}), \
                         treating as workflow"
                    ));
                    (Vec::new(), None)
                }
            }
        } else {
            (Vec::new(), None)
        };

        // ── Parse workflow only when input came from a file (-f/--file) ───────
        // Stdin is never treated as workflow markdown; only explicit workflow
        // files get ## steps, preamble, and H1 title.
        let workflow = if opts.input_from_file {
            Some(parse_workflow(markdown_body))
        } else {
            None
        };

        // Frontmatter title takes priority over H1; H1 is the fallback (file only).
        let title = frontmatter
            .title
            .or(workflow.as_ref().and_then(|w| w.title.clone()));

        // Workflow preamble → system prompt (only when input was from a workflow file).
        // Computed before building the queue so we can use workflow.as_ref() (queue consumes it).
        let workflow_system_prompt_append = if opts.input_from_file
            && !opts.input.trim().is_empty()
            && !is_conversation_input
            && !is_jsonl_input
            && !is_json_summary_input
        {
            workflow
                .as_ref()
                .and_then(|w| w.system_prompt_append.clone())
        } else {
            None
        };
        let combined_append = match (
            workflow_system_prompt_append,
            opts.append_system_prompt.clone(),
        ) {
            (Some(p), Some(a)) => Some(format!("{p}\n\n{a}")),
            (p, a) => p.or(a),
        };

        // ── Build step queue ─────────────────────────────────────────────────
        let mut queue: StepQueue = if opts.input.trim().is_empty() {
            // No input at all - use the positional prompt as the sole step.
            let content = opts.extra_prompt.clone().unwrap_or_default();
            StepQueue::from(vec![Step {
                label: None,
                content,
                options: Default::default(),
            }])
        } else if is_conversation_input || is_jsonl_input || is_json_summary_input {
            // Piped conversation/JSONL/JSON-summary: do not treat as workflow.
            //
            // Step content priority:
            //   1. CLI positional prompt   (explicit task for the new turn)
            //   2. Trailing pending user   (last ## User section without a response,
            //                              only available for conversation/JSONL input)
            //   3. Error - nothing to do
            let content = opts.extra_prompt.clone().or(piped_pending.clone());
            match content {
                Some(c) => StepQueue::from(vec![Step {
                    label: None,
                    content: c,
                    options: Default::default(),
                }]),
                None => {
                    let format_name = if is_jsonl_input {
                        "JSONL"
                    } else if is_json_summary_input {
                        "JSON summary"
                    } else {
                        "conversation"
                    };
                    write_stderr(&format!(
                        "[sven:error] Piped {format_name} has no pending task.\n\
                         \n\
                         To continue a piped conversation provide a prompt:\n\
                         \n\
                         \tsven 'task1' | sven 'task2'\n\
                         \n\
                         Or end the piped output with an unanswered ## User section\n\
                         so the next sven instance picks it up automatically."
                    ));
                    std::process::exit(EXIT_VALIDATION_ERROR);
                }
            }
        } else if let Some(w) = workflow {
            // Workflow file: use parsed ## steps and optional CLI prompt prepend.
            let mut q = w.steps;
            if let Some(prompt) = &opts.extra_prompt {
                let mut prepended = StepQueue::from(vec![Step {
                    label: None,
                    content: prompt.clone(),
                    options: Default::default(),
                }]);
                while let Some(s) = q.pop() {
                    prepended.push(s);
                }
                prepended
            } else {
                q
            }
        } else {
            // Stdin (no -f): plain text as a single step; no workflow parsing.
            // If a positional prompt was also supplied (e.g. `echo "data" | sven "analyse"`),
            // prepend it so both the context and the task appear in the same user message.
            let body = markdown_body.trim().to_string();
            let content = match &opts.extra_prompt {
                Some(p) if !body.is_empty() => format!("{}\n\n{}", p.trim(), body),
                Some(p) => p.trim().to_string(),
                None => body,
            };
            StepQueue::from(vec![Step {
                label: None,
                content,
                options: Default::default(),
            }])
        };

        let total = queue.len();

        // ── Dry-run mode ─────────────────────────────────────────────────────
        if opts.dry_run {
            write_progress(&format!(
                "[sven:dry-run] Workflow validated - {} step(s)",
                total
            ));
            if let Some(t) = &title {
                write_progress(&format!("[sven:dry-run] Title: {}", t));
            }
            let mut i = 0;
            while let Some(step) = queue.pop() {
                i += 1;
                let label = step.label.as_deref().unwrap_or("(unlabelled)");
                let mode_hint = step.options.mode.as_deref().unwrap_or("(inherit)");
                let provider_hint = step.options.provider.as_deref().unwrap_or("(inherit)");
                let model_hint = step.options.model.as_deref().unwrap_or("(inherit)");
                let timeout_hint = step
                    .options
                    .timeout_secs
                    .map(|t| format!("{t}s"))
                    .unwrap_or_else(|| "(inherit)".to_string());
                write_progress(&format!(
                    "[sven:dry-run] Step {i}/{total}: label={label:?} mode={mode_hint} provider={provider_hint} model={model_hint} timeout={timeout_hint}"
                ));
            }
            return Ok(());
        }

        // ── Build model config ───────────────────────────────────────────────
        // CLI --model > frontmatter models[current_mode] > config
        let model_override = opts.model_override.clone().or_else(|| {
            frontmatter
                .models
                .as_ref()
                .and_then(|m| m.get("agent"))
                .cloned()
        });
        let model_cfg = if let Some(ref name) = model_override {
            sven_model::resolve_model_from_config(&self.config, name)
        } else {
            self.config.model.clone()
        };

        // Validate the model provider builds now so a bad `--model` / config
        // fails fast with the same error the legacy runner surfaced before
        // constructing the agent. The kernel session (re)builds the provider
        // from `model_cfg` per turn.
        let _ = sven_model_drivers::from_config(&model_cfg)
            .context("failed to initialise model provider")?;

        write_stderr(&format!(
            "[sven:settings] model={} mode={}",
            model_cfg.name, opts.mode
        ));

        // (turn_metadata removed - Conversation output now streams in real-time;
        // no post-step metadata serialization needed)

        // ── Build runtime context ─────────────────────────────────────────────
        // Only ever consumed by the built-in system prompt (see
        // `AgentRuntimeContext::build_system_message`), which `--no-system`
        // bypasses entirely, so skip reading and logging it in that case.
        let project_context_file = if opts.no_system {
            None
        } else {
            opts.project_root
                .as_ref()
                .and_then(|r| sven_workspace::find_project_context_file(r))
        };

        if let Some(path) = &project_context_file {
            write_progress(&format!(
                "[sven:info] Project context file found at {} (referenced, not auto-loaded)",
                path.display()
            ));
        }

        let mut runtime_ctx = RuntimeContext {
            ci_context: Some(ci_ctx),
            project_context_file,
            append_system_prompt: combined_append,
            system_prompt_override: self.config.agent.system_prompt.clone(),
            no_system: opts.no_system,
            no_tools: opts.no_tools,
            ..RuntimeContext::auto_detect_at(opts.project_root.clone())
        };

        // ── --system-prompt-file override ────────────────────────────────────
        if let Some(sp_file) = &opts.system_prompt_file {
            match std::fs::read_to_string(sp_file) {
                Ok(content) => {
                    runtime_ctx.system_prompt_override = Some(content.trim().to_string());
                    write_progress(&format!(
                        "[sven:info] System prompt loaded from {}",
                        sp_file.display()
                    ));
                }
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:error] Failed to read --system-prompt-file {}: {e}",
                        sp_file.display()
                    ));
                    std::process::exit(EXIT_VALIDATION_ERROR);
                }
            }
        }

        // ── Pre-load an ATIF trajectory (if --load-trace/--trace was given) ────
        // Parsed early (before the agent seeds history) so --rerun-toolcalls
        // can mutate the loaded steps' observations before they're replayed
        // into the agent's seeded history. Unlike the old JSONL path, there is
        // no stored-system-prompt injection here: ATIF trajectories never
        // persist the system prompt as a step (see
        // `sven_session_store::trace_session::StepAssembler::push_message`'s
        // System-role handling) — the agent always regenerates it fresh, so
        // `--regen-system-prompt` has no additional effect on trace-loaded
        // runs (see its doc comment on `CiOptions`).
        //
        // A missing `--load-trace`/`--trace` path is treated as "nothing to
        // load yet", not an error, so `--trace PATH` on a fresh path creates
        // the file on first write rather than failing the run.
        let (mut existing_steps, existing_session_id, existing_meta, loaded_subagents): (
            Vec<TraceStep>,
            Option<String>,
            Option<SvenSessionMeta>,
            Vec<Trajectory>,
        ) = match &opts.load_trace {
            Some(tpath) if tpath.exists() => match trace_session::load_session_from(tpath) {
                Ok(trajectory) => {
                    let meta = SvenSessionMeta::from_trajectory(&trajectory);
                    // Preserve embedded children across a resume: `flush_trace`
                    // below only ever rebuilds `subagent_trajectories` from
                    // *this run's* `completed_subagents`, so a loaded
                    // trajectory's own children must be seeded into it up
                    // front or the very first flush of a resumed session
                    // silently drops every subagent it had.
                    let subagents = trajectory.subagent_trajectories.unwrap_or_default();
                    (trajectory.steps, trajectory.session_id, meta, subagents)
                }
                Err(e) => {
                    write_stderr(&format!(
                        "[sven:error] Failed to load --load-trace {}: {e:#}",
                        tpath.display()
                    ));
                    std::process::exit(EXIT_VALIDATION_ERROR);
                }
            },
            _ => (Vec::new(), None, None, Vec::new()),
        };

        // Resolve timeouts (CLI > config)
        // Frontmatter no longer carries timeout fields (removed in redesign).
        let run_timeout_secs = opts.run_timeout_secs.or_else(|| {
            if self.config.agent.max_run_timeout_secs > 0 {
                Some(self.config.agent.max_run_timeout_secs)
            } else {
                None
            }
        });

        let global_step_timeout_secs = opts.step_timeout_secs.or_else(|| {
            if self.config.agent.max_step_timeout_secs > 0 {
                Some(self.config.agent.max_step_timeout_secs)
            } else {
                None
            }
        });

        // Resolve mode from CLI (frontmatter no longer carries a top-level mode)
        let initial_mode = opts.mode;

        // ── Build the kernel-backed agent ────────────────────────────────────
        // Each turn runs on a freshly-built HSM kernel session (via
        // RuntimeBuilder) seeded with the accumulated history, so per-step mode
        // and model overrides are honoured while the streaming AgentEvent
        // contract this runner consumes stays identical.
        let mut agent = KernelAgent::new(
            self.config.clone(),
            runtime_ctx,
            initial_mode,
            model_cfg.clone(),
        );

        let seed_count = if !existing_steps.is_empty() {
            // Loaded ATIF trajectory (--load-trace/--trace path).
            if opts.rerun_toolcalls {
                let replay_tools = agent.build_tool_registry()?;
                let replayed =
                    crate::toolcall_replay::replay_tool_calls(&mut existing_steps, &replay_tools)
                        .await;
                write_progress(&format!(
                    "[sven:info] Replayed {} tool call(s) with fresh results",
                    replayed
                ));
            }
            let history_msgs = trace_session::steps_to_messages(&existing_steps);
            let count = history_msgs.len();
            agent.seed_history(history_msgs);
            count
        } else if !conversation_history.is_empty() {
            // Piped markdown/JSONL conversation (legacy path)
            let count = conversation_history.len();
            agent.seed_history(conversation_history);
            count
        } else {
            0
        };

        if seed_count > 0 {
            write_progress(&format!(
                "[sven:info] Loaded {seed_count} prior message(s) into conversation history"
            ));
        }

        // ── Resolve effective trace output path ──────────────────────────────
        // Priority: --output-trace > auto-log path.
        // Note: --load-trace alone does NOT imply write-back to the same file;
        // use --trace (which sets both) for that behaviour.  The auto-log is
        // always the fallback so there is always a trace record of the run.
        let effective_output_trace: Option<PathBuf> =
            opts.output_trace.clone().or_else(resolve_auto_log_path);

        // ── Turn assembler for this run's new steps ──────────────────────────
        // Continues the step_id sequence from whatever was loaded above, so
        // the combined `existing_steps ++ assembler` sequence stays
        // contiguous starting at 1 (required by `atif::validate_trajectory`).
        let mut assembler = StepAssembler::resuming(existing_steps.len() as u64 + 1);
        // Number of `assembler.closed_steps()` already streamed to stdout;
        // only advances when `output_format == Jsonl`, see `event::stream_new_steps`.
        let mut emitted_steps: usize = 0;
        // Session identity for the trajectory this run writes: reuse the
        // loaded session (continuing it), or - for a fresh session with an
        // explicit --output-trace/--trace path - adopt the file's own stem.
        // `session_resolve::resolve_session_id` (which `sven chats`/`--resume`
        // use) matches on filename, not this field, so a session_id that
        // disagrees with its own filename would make `sven chats` print an id
        // `--resume` can never resolve back to this file. Falls back to a
        // fresh UUID only when there is no explicit output path to derive one
        // from (e.g. the auto-log path, which is not meant to be looked up).
        let run_session_id = existing_session_id.unwrap_or_else(|| {
            opts.output_trace
                .as_ref()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(trace_session::new_session_id)
        });

        // ── Set up Ctrl+C handler ────────────────────────────────────────────
        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = cancel_tx.send(()).await;
            }
        });

        // ── Output: emit title for conversation format ───────────────────────
        if opts.output_format == OutputFormat::Conversation {
            if let Some(t) = &title {
                write_stdout(&format!("# {}\n\n", t));
            }
        }

        // ── Artifacts setup ──────────────────────────────────────────────────
        if let Some(dir) = &opts.artifacts_dir {
            if let Err(e) = std::fs::create_dir_all(dir) {
                write_stderr(&format!("[sven:warn] Could not create artifacts dir: {e}"));
            }
        }

        // ── Cache directory for cache_key step skipping ──────────────────────
        let cache_dir: PathBuf = opts
            .project_root
            .as_deref()
            .map(|r| r.join(".sven").join("cache"))
            .unwrap_or_else(|| PathBuf::from(".sven/cache"));

        // ── Run step loop ────────────────────────────────────────────────────
        let run_start = Instant::now();
        let mut step_idx = 0usize;
        let mut collected: Vec<Message> = Vec::new();
        // Running session-level token counters for [sven:tokens] output.
        let mut session_input_total: u32 = 0;
        let mut session_output_total: u32 = 0;

        // Cross-step tracking for exit code decisions.
        let mut any_tool_errors: bool = false;
        let mut run_total_tokens: u64 = 0;
        // Outcome tally for the reward stamped on the concluded trajectory.
        // Spans the whole run (not per-step), like `subagent_children` below,
        // because the reward scores the session as a whole.
        let mut outcome_fold = OutcomeFold::default();
        let max_tokens_budget = opts.max_tokens_budget;

        // In-progress `task`-tool subagents (keyed by ACP `handle_id`) and
        // the trajectories of those that have finished so far — both
        // threaded through every `StepState` for the life of the run (not
        // reset per-step) so a subagent spawned in one step and finished in
        // a later one still resolves correctly. See `event::SubagentChildState`
        // and `StepState::completed_subagents`.
        let mut subagent_children: HashMap<String, SubagentChildState> = HashMap::new();
        let mut completed_subagents: Vec<Trajectory> = loaded_subagents;

        // Write the combined ATIF trajectory (existing steps ++ new steps
        // accumulated so far) to `path`, atomically (temp file + rename; see
        // `atif::persist::write_trajectory_atomic`) so a crash never leaves
        // a half-written document. `expected: None` — same as the old JSONL
        // flush, this run is the sole writer for the duration and does not
        // need concurrent-modification detection against other processes.
        // `subagent_trajectories` — like `new_steps` — is passed as a
        // parameter rather than captured by reference: both `assembler` and
        // `completed_subagents` keep mutating after this closure is defined,
        // so each call site passes a fresh snapshot instead of the closure
        // holding a live (and therefore borrow-conflicting) reference.
        // `outcome` is what makes a flush a CONCLUSION: `Some` stamps
        // `SessionOutcome` onto the trajectory (a real reward if `Scored`, an
        // explicit unknown-reason if `Unknown` - either way declaring the
        // outcome final); `None` leaves the document untouched, which
        // downstream trajectory consumers read as "outcome unknown" and
        // skip. It is a parameter rather than a captured flag so the
        // compiler forces every flush site — including ones added later —
        // to say which it is.
        //
        // Note this closure rebuilds the trajectory from scratch, so with
        // `--load-trace` a predecessor run's outcome is dropped rather than
        // inherited. That is intended: the outcome describes *this* run.
        let flush_trace = |path: &PathBuf,
                           new_steps: &[TraceStep],
                           subagent_trajectories: &[Trajectory],
                           outcome: Option<&SessionOutcome>| {
            let agent_profile = trace_session::default_agent_profile()
                .with_model(format!("{}/{}", model_cfg.provider, model_cfg.name));
            let mut trajectory = Trajectory::new(trace_session::ATIF_SCHEMA_VERSION, agent_profile);
            trajectory.session_id = Some(run_session_id.clone());
            trajectory.steps = existing_steps
                .iter()
                .cloned()
                .chain(new_steps.iter().cloned())
                .collect();
            if !subagent_trajectories.is_empty() {
                trajectory.subagent_trajectories = Some(subagent_trajectories.to_vec());
            }

            let mut meta = existing_meta.clone().unwrap_or_else(|| {
                SvenSessionMeta::new(title.clone().unwrap_or_else(|| "CI Run".to_string()))
            });
            meta.touch();
            meta.mode = Some(opts.mode.to_string());
            meta.apply_to_trajectory(&mut trajectory);
            if let Some(outcome) = outcome {
                apply_outcome_to_trajectory(&mut trajectory, outcome);
            }

            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = atif::persist::write_trajectory_atomic(path, &trajectory, None) {
                eprintln!(
                    "[sven:warn] Failed to write trace log {}: {e}",
                    path.display()
                );
            }
        };

        // Consumed by the first step only: `--attach` decorates the initial
        // user turn, not every step.
        let mut pending_attachments: Vec<PathBuf> = opts.attachments.clone();

        while let Some(step) = queue.pop() {
            step_idx += 1;
            let label = step.label.as_deref().unwrap_or("(unlabelled)");

            // Check total run timeout (between steps)
            if let Some(t) = run_timeout_secs {
                if run_start.elapsed() > Duration::from_secs(t) {
                    write_stderr(&format!(
                        "[sven:error] Total run timeout exceeded ({}s). Completed {}/{} steps.",
                        t,
                        step_idx - 1,
                        total
                    ));
                    // Record the timed-out run rather than exiting with no
                    // document: the previous step's flush left the trajectory
                    // unstamped, which would otherwise read as "still running".
                    if let Some(ref path) = effective_output_trace {
                        flush_trace(
                            path,
                            &assembler.snapshot_including_pending(),
                            &completed_subagents,
                            Some(&outcome_fold.conclude(RunConclusion::Timeout, None)),
                        );
                    }
                    std::process::exit(EXIT_TIMEOUT);
                }
            }

            // Apply per-step mode override
            if let Some(mode_str) = &step.options.mode {
                if let Some(mode) = parse_agent_mode(mode_str) {
                    agent.set_mode(mode);
                } else {
                    write_stderr(&format!(
                        "[sven:warn] Unknown mode {:?} in step {step_idx}, continuing with current mode",
                        mode_str
                    ));
                }
            }

            // Apply per-step provider and/or model override.
            // Priority: explicit step model/provider > frontmatter models[mode] > current model.
            let fm_mode_model: Option<String> = step
                .options
                .mode
                .as_deref()
                .and_then(|m| frontmatter.models.as_ref()?.get(m).cloned())
                .or_else(|| frontmatter.models.as_ref()?.get("agent").cloned());

            let effective_model_str: Option<String> = match (
                step.options.provider.as_deref(),
                step.options.model.as_deref(),
            ) {
                (Some(prov), Some(model)) => Some(format!("{prov}/{model}")),
                (Some(prov), None) => Some(prov.to_string()),
                (None, Some(model)) => Some(model.to_string()),
                (None, None) => fm_mode_model,
            };
            if let Some(model_str) = &effective_model_str {
                let step_model_cfg = sven_model::resolve_model_from_config(&self.config, model_str);
                // Validate the override builds before switching; on failure keep
                // the current model (mirrors the legacy runner's warn-and-continue).
                match sven_model_drivers::from_config(&step_model_cfg) {
                    Ok(_) => {
                        agent.set_model_config(step_model_cfg);
                    }
                    Err(e) => {
                        write_stderr(&format!(
                            "[sven:warn] Failed to build model {model_str:?} for step {step_idx}: {e}, using current model"
                        ));
                    }
                }
            }

            // Resolve step timeout
            let step_timeout_secs = step.options.timeout_secs.or(global_step_timeout_secs);

            if let Some(l) = step.label.as_deref() {
                write_progress(&format!(
                    "[sven:step:start] {}/{} label=\"{}\"",
                    step_idx, total, l
                ));
            } else {
                write_progress(&format!("[sven:step:start] {}/{}", step_idx, total));
            }

            let step_start = Instant::now();

            // Apply variable substitution
            let step_content = if !vars.is_empty() {
                apply_template(&step.content, &vars)
            } else {
                step.content.clone()
            };

            // Mark where this step's messages begin in `collected`
            let step_msg_start = collected.len();

            // ── --attach: pre-load attachments into the first user turn ─────
            // No tool call is involved, so this works even with models that
            // cannot call tools at all.
            let attached_parts: Option<Vec<sven_model::ContentPart>> = if pending_attachments
                .is_empty()
            {
                None
            } else {
                let paths = std::mem::take(&mut pending_attachments);
                // The agent's *current* model: a per-step override may
                // already have replaced the initial one, and its
                // modalities are what decide native vs. transcribed audio.
                let model = agent
                    .model()
                    .context("failed to initialise model provider for --attach")?;
                Some(
                    build_attachment_parts(&step_content, &paths, &model, &self.config.tools.asr)
                        .await?,
                )
            };

            // Record the user turn before submitting. A user message always
            // closes any pending agent step from the *previous* CI step (see
            // `StepAssembler::push_message`), which is what makes the
            // flush-after-every-step behaviour below correct even though the
            // previous step's final turn may not have been closed by
            // anything else yet.
            let user_msg = match &attached_parts {
                Some(parts) => Message::user_with_parts(parts.clone()),
                None => Message::user(&step_content),
            };
            collected.push(user_msg.clone());
            assembler.push_message(&user_msg);
            stream_new_steps(&assembler, &mut emitted_steps, opts.output_format);

            // In streaming conversation format emit step label (if any) and ## User section
            if opts.output_format == OutputFormat::Conversation {
                if step.label.as_deref().is_some_and(|l| !l.is_empty()) {
                    write_stdout(&format!("## {label}\n\n## User\n{step_content}\n\n"));
                } else {
                    write_stdout(&format!("## User\n{step_content}\n\n"));
                }
            }

            // Per-step output accumulators - declared here so both the cache-hit
            // path and the agent path share the same downstream output logic.
            let mut response_text = String::new();
            let mut tools_used: Vec<String> = Vec::new();
            let mut failed = false;
            let mut step_duration_ms = 0u64;
            // Tracks whether the `## Sven\n` header has been emitted for streaming.
            let mut sven_header_emitted = false;

            // ── cache_key: skip agent call if cached output exists ────────────
            // Keys are sanitized before building a filesystem path to prevent
            // any path-traversal via malicious or accidental key values.
            let cache_hit = 'cache: {
                if let Some(ref key) = step.options.cache_key {
                    let safe_key = sanitize_cache_key(key);
                    let cache_path = cache_dir.join(format!("{}.md", safe_key));
                    if cache_path.exists() {
                        if let Ok(cached) = std::fs::read_to_string(&cache_path) {
                            write_progress(&format!(
                                "[sven:cache:hit] {}/{} key={:?} path={}",
                                step_idx,
                                total,
                                key,
                                cache_path.display()
                            ));
                            collected.push(Message::assistant(&cached));
                            response_text = cached;
                            break 'cache true;
                        }
                    }
                }
                false
            };

            // Run the agent only when there was no cache hit.
            if !cache_hit {
                let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
                // Boxed so both submit variants share one future type.
                type SubmitFuture<'a> =
                    std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + 'a>>;
                let submit_fut: SubmitFuture<'_> = match attached_parts {
                    Some(parts) => Box::pin(agent.submit_with_parts(parts, tx)),
                    None => Box::pin(agent.submit(&step_content, tx)),
                };

                let mut consecutive_tool_errors = 0;
                // Set by `handle_event` when `--max-tokens` is exceeded; see
                // `StepState::budget_exhausted`'s doc comment for why this is
                // a flag checked here rather than a direct `process::exit`
                // inside the event handler.
                let mut budget_exhausted = false;

                tokio::pin!(submit_fut);

                // Build a step-level timeout future.
                // If no timeout set, use a future that never resolves.
                let step_timeout_fut = async {
                    if let Some(t) = step_timeout_secs {
                        tokio::time::sleep(Duration::from_secs(t)).await;
                        true // timed out
                    } else {
                        futures::future::pending::<bool>().await
                    }
                };
                tokio::pin!(step_timeout_fut);

                loop {
                    tokio::select! {
                        biased;

                        timed_out = &mut step_timeout_fut => {
                            if timed_out {
                                write_stderr(&format!(
                                    "[sven:error] Step {step_idx} ({label:?}) timed out after {}s",
                                    step_timeout_secs.unwrap_or(0)
                                ));
                                if let Some(ref path) = effective_output_trace {
                                    flush_trace(
                                        path,
                                        &assembler.snapshot_including_pending(),
                                        &completed_subagents,
                                        Some(&outcome_fold.conclude(RunConclusion::Timeout, None)),
                                    );
                                }
                                std::process::exit(EXIT_TIMEOUT);
                            }
                        }

                        _ = cancel_rx.recv() => {
                            write_stderr("[sven:interrupted] Ctrl+C received - saving partial conversation");
                            if let Some(ref path) = effective_output_trace {
                                flush_trace(
                                    path,
                                    &assembler.snapshot_including_pending(),
                                    &completed_subagents,
                                    Some(&outcome_fold.conclude(RunConclusion::Cancelled, None)),
                                );
                            }
                            std::process::exit(EXIT_INTERRUPT);
                        }

                        Some(event) = rx.recv() => {
                            handle_event(event, &mut StepState {
                                response_text: &mut response_text,
                                tools_used: &mut tools_used,
                                failed: &mut failed,
                                collected: &mut collected,
                                assembler: &mut assembler,
                                emitted_steps: &mut emitted_steps,
                                consecutive_tool_errors: &mut consecutive_tool_errors,
                                trace_level: opts.trace_level,
                                output_format: opts.output_format,
                                sven_header_emitted: &mut sven_header_emitted,
                                session_input_total: &mut session_input_total,
                                session_output_total: &mut session_output_total,
                                any_tool_errors: &mut any_tool_errors,
                                run_total_tokens: &mut run_total_tokens,
                                max_tokens_budget,
                                budget_exhausted: &mut budget_exhausted,
                                subagent_children: &mut subagent_children,
                                completed_subagents: &mut completed_subagents,
                                outcome: &mut outcome_fold,
                            });

                            // Abort if the `--max-tokens` budget was exhausted.
                            // Flushing here (rather than in `handle_event`) is
                            // what makes `--output-trace` still get a valid
                            // partial document when this fires — see
                            // `StepState::budget_exhausted`'s doc comment.
                            if budget_exhausted {
                                if let Some(ref path) = effective_output_trace {
                                    flush_trace(
                                        path,
                                        &assembler.snapshot_including_pending(),
                                        &completed_subagents,
                                        Some(
                                            &outcome_fold
                                                .conclude(RunConclusion::BudgetExhausted, None),
                                        ),
                                    );
                                }
                                std::process::exit(EXIT_BUDGET_EXHAUSTED);
                            }

                            // Abort if too many consecutive tool errors
                            const MAX_CONSECUTIVE_TOOL_ERRORS: u32 = 20;
                            if consecutive_tool_errors >= MAX_CONSECUTIVE_TOOL_ERRORS {
                                write_stderr(&format!(
                                    "[sven:fatal] Step {step_idx} ({label:?}) aborted: \
                                     {MAX_CONSECUTIVE_TOOL_ERRORS} consecutive tool errors. \
                                     This often indicates the model is using wrong parameter names \
                                     or is confused. Consider using a more capable model."
                                ));
                                if let Some(ref path) = effective_output_trace {
                                    flush_trace(
                                        path,
                                        &assembler.snapshot_including_pending(),
                                        &completed_subagents,
                                        Some(&outcome_fold.conclude(RunConclusion::AgentError, None)),
                                    );
                                }
                                std::process::exit(EXIT_AGENT_ERROR);
                            }
                        }

                        result = &mut submit_fut => {
                            if let Err(e) = result {
                                write_stderr(&format!(
                                    "[sven:fatal] Step {step_idx} ({label:?}) failed: {e:#}"
                                ));
                                // A hard submit failure is exactly the kind of
                                // negative outcome the trajectory should record,
                                // so flush before exiting rather than leaving no
                                // document at all.
                                if let Some(ref path) = effective_output_trace {
                                    flush_trace(
                                        path,
                                        &assembler.snapshot_including_pending(),
                                        &completed_subagents,
                                        Some(&outcome_fold.conclude(RunConclusion::AgentError, None)),
                                    );
                                }
                                std::process::exit(EXIT_AGENT_ERROR);
                            }
                            while let Ok(ev) = rx.try_recv() {
                                handle_event(ev, &mut StepState {
                                    response_text: &mut response_text,
                                    tools_used: &mut tools_used,
                                    failed: &mut failed,
                                    collected: &mut collected,
                                    assembler: &mut assembler,
                                    emitted_steps: &mut emitted_steps,
                                    consecutive_tool_errors: &mut consecutive_tool_errors,
                                    trace_level: opts.trace_level,
                                    output_format: opts.output_format,
                                    sven_header_emitted: &mut sven_header_emitted,
                                    session_input_total: &mut session_input_total,
                                    session_output_total: &mut session_output_total,
                                    any_tool_errors: &mut any_tool_errors,
                                    run_total_tokens: &mut run_total_tokens,
                                    max_tokens_budget,
                                    budget_exhausted: &mut budget_exhausted,
                                    subagent_children: &mut subagent_children,
                                    completed_subagents: &mut completed_subagents,
                                    outcome: &mut outcome_fold,
                                });
                                if budget_exhausted {
                                    break;
                                }
                            }
                            if budget_exhausted {
                                if let Some(ref path) = effective_output_trace {
                                    flush_trace(
                                        path,
                                        &assembler.snapshot_including_pending(),
                                        &completed_subagents,
                                        Some(
                                            &outcome_fold
                                                .conclude(RunConclusion::BudgetExhausted, None),
                                        ),
                                    );
                                }
                                std::process::exit(EXIT_BUDGET_EXHAUSTED);
                            }
                            break;
                        }
                    }
                }

                step_duration_ms = step_start.elapsed().as_millis() as u64;

                // ── Write to cache after a successful agent run ───────────────
                if let Some(ref key) = step.options.cache_key {
                    if !failed && !response_text.is_empty() {
                        let safe_key = sanitize_cache_key(key);
                        let cache_path = cache_dir.join(format!("{}.md", safe_key));
                        if let Some(parent) = cache_path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        match std::fs::write(&cache_path, &response_text) {
                            Ok(()) => write_progress(&format!(
                                "[sven:cache:write] key={:?} path={}",
                                key,
                                cache_path.display()
                            )),
                            Err(e) => write_stderr(&format!(
                                "[sven:warn] Failed to write cache {}: {e}",
                                cache_path.display()
                            )),
                        }
                    }
                }
            }

            // ── Inject step output into template vars for subsequent steps ───
            // Makes {{step.<label>.output}} and {{step.<N>.output}} available
            // in all following steps without any file I/O.
            let norm = normalize_label(label);
            vars.insert(format!("step.{}.output", norm), response_text.clone());
            vars.insert(format!("step.{}.output", step_idx), response_text.clone());

            // ── Flush the trace after every step ─────────────────────────────
            // Mid-run, so deliberately unstamped: the run has not concluded and
            // later steps may still fail.
            if let Some(ref path) = effective_output_trace {
                flush_trace(
                    path,
                    &assembler.snapshot_including_pending(),
                    &completed_subagents,
                    None,
                );
            }

            // ── Write step output to stdout ──────────────────────────────────
            match opts.output_format {
                OutputFormat::Conversation => {
                    // Streaming output: already emitted in handle_event / step start above.
                    // Nothing more to write here for Conversation format.
                }
                OutputFormat::Jsonl => {
                    // Streaming output: each closed step was already emitted to
                    // stdout by `stream_new_steps()` as it closed.  Nothing more
                    // to write here.
                }
                OutputFormat::Compact => {
                    if !response_text.ends_with('\n') {
                        write_stdout(&format!("{response_text}\n"));
                    } else {
                        write_stdout(&response_text);
                    }
                }
                OutputFormat::Json => {
                    // The full trajectory (including this step's contribution)
                    // is written once at the very end of the run, once the
                    // assembler is finished — see the final `OutputFormat::Json`
                    // block below.
                }
            }

            // ── Write per-step artifact ──────────────────────────────────────
            if let Some(dir) = &opts.artifacts_dir {
                write_step_artifact(dir, step_idx, label, &collected[step_msg_start..]);
            }

            // ── Progress report ──────────────────────────────────────────────
            let cache_suffix = if cache_hit { " (cached)" } else { "" };
            if let Some(l) = step.label.as_deref() {
                write_progress(&format!(
                    "[sven:step:complete] {}/{} label=\"{}\" duration_ms={} tools={} success={}{}",
                    step_idx,
                    total,
                    l,
                    step_duration_ms,
                    tools_used.len(),
                    !failed,
                    cache_suffix
                ));
            } else {
                write_progress(&format!(
                    "[sven:step:complete] {}/{} duration_ms={} tools={} success={}{}",
                    step_idx,
                    total,
                    step_duration_ms,
                    tools_used.len(),
                    !failed,
                    cache_suffix
                ));
            }

            if failed {
                write_stderr(&format!(
                    "[sven:error] Step {step_idx} ({label:?}) reported an error. Aborting."
                ));
                if let Some(ref path) = effective_output_trace {
                    flush_trace(
                        path,
                        &assembler.snapshot_including_pending(),
                        &completed_subagents,
                        Some(&outcome_fold.conclude(RunConclusion::AgentError, None)),
                    );
                }
                std::process::exit(EXIT_AGENT_ERROR);
            }

            if step_idx < total {
                write_stderr(&format!("\n--- step {}/{} complete ---\n", step_idx, total));
            }
        }

        // ── Finish the assembler ─────────────────────────────────────────────
        // Flushes any still-open pending agent step (e.g. the very last turn's
        // trailing assistant text, which nothing closed yet) into a real step.
        // Any steps closed by this that hadn't been streamed yet (Jsonl format)
        // are emitted now, so the NDJSON stream always ends with every step.
        let new_steps = assembler.finish();
        if opts.output_format == OutputFormat::Jsonl {
            for step in &new_steps[emitted_steps..] {
                match serde_json::to_string(step) {
                    Ok(line) => write_stdout(&format!("{line}\n")),
                    Err(e) => {
                        write_stderr(&format!("[sven:warn] Failed to serialize trace step: {e}"))
                    }
                }
            }
        }
        let final_trajectory_steps: Vec<TraceStep> = existing_steps
            .iter()
            .cloned()
            .chain(new_steps.iter().cloned())
            .collect();

        // ── Final trace flush ────────────────────────────────────────────────
        // Ensure the last step is persisted even if no prior flush fired.
        let final_outcome = outcome_fold.conclude(RunConclusion::Success, None);
        if let Some(ref path) = effective_output_trace {
            flush_trace(path, &new_steps, &completed_subagents, Some(&final_outcome));
            write_progress(&format!("[sven:trace] Trace written to {}", path.display()));
        }

        // ── Finalize JSON output ─────────────────────────────────────────────
        // Emit the run's full ATIF trajectory as pretty JSON — replaces the
        // old bespoke {title, steps:[{user_input, agent_response, ...}]}
        // summary with the actual trace document (also what --output-trace
        // would have written to a file).
        if opts.output_format == OutputFormat::Json {
            let agent_profile = trace_session::default_agent_profile()
                .with_model(format!("{}/{}", model_cfg.provider, model_cfg.name));
            let mut trajectory = Trajectory::new(trace_session::ATIF_SCHEMA_VERSION, agent_profile);
            trajectory.session_id = Some(run_session_id.clone());
            trajectory.steps = final_trajectory_steps.clone();
            if !completed_subagents.is_empty() {
                trajectory.subagent_trajectories = Some(completed_subagents.clone());
            }
            let mut meta = existing_meta.clone().unwrap_or_else(|| {
                SvenSessionMeta::new(title.clone().unwrap_or_else(|| "CI Run".to_string()))
            });
            meta.touch();
            meta.mode = Some(opts.mode.to_string());
            meta.apply_to_trajectory(&mut trajectory);
            // Same document as `--output-trace` would have written, so it
            // carries the same conclusion stamp.
            apply_outcome_to_trajectory(&mut trajectory, &final_outcome);

            match serde_json::to_string_pretty(&trajectory) {
                Ok(json) => write_stdout(&format!("{json}\n")),
                Err(e) => write_stderr(&format!("[sven:warn] Failed to serialize trajectory: {e}")),
            }
        }

        // ── --output-last-message ─────────────────────────────────────────────
        if let Some(out_path) = &opts.output_last_message {
            // Extract the last assistant response from the collected messages.
            let last_response = collected
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant)
                .and_then(|m| match &m.content {
                    MessageContent::Text(t) => Some(t.clone()),
                    MessageContent::ContentParts(parts) => {
                        let text: String = parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text { text } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("");
                        if text.is_empty() {
                            None
                        } else {
                            Some(text)
                        }
                    }
                    _ => None,
                });

            if let Some(text) = last_response {
                if let Some(parent) = out_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                match std::fs::write(out_path, &text) {
                    Ok(()) => write_progress(&format!(
                        "[sven:info] Last message written to {}",
                        out_path.display()
                    )),
                    Err(e) => write_stderr(&format!(
                        "[sven:warn] Could not write --output-last-message {}: {e}",
                        out_path.display()
                    )),
                }
            }
        }

        // ── Save artifacts metadata ──────────────────────────────────────────
        if let Some(dir) = &opts.artifacts_dir {
            write_conversation_artifact(dir, &collected);
        }

        // ── Exit with tool-warning code if any non-fatal tool errors occurred ─
        // Exit code 3 signals "run completed but with tool warnings" - the
        // caller can use this to distinguish a clean run from a partially
        // successful one without treating it as a hard failure.
        if any_tool_errors {
            write_stderr("[sven:warn] Run completed with tool errors (exit 3).");
            std::process::exit(EXIT_TOOL_WARNINGS);
        }

        Ok(())
    }
}
