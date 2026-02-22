use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentMode, Config};
use sven_core::AgentEvent;
use sven_bootstrap::{AgentBuilder, RuntimeContext, ToolSetProfile};
use sven_input::{
    history, parse_frontmatter, parse_markdown_steps, extract_h1_title,
    serialize_conversation_turn, Step, StepQueue,
};
use sven_model::{FunctionCall, Message, MessageContent, Role};
use sven_tools::events::TodoItem;

use crate::output::{write_stderr, write_stdout, write_progress};
use crate::template::apply_template;

// ── Exit codes ────────────────────────────────────────────────────────────────

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_AGENT_ERROR: i32 = 1;
pub const EXIT_VALIDATION_ERROR: i32 = 2;
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
    /// Structured JSON: one JSON object per run with step metadata.
    Json,
    /// Compact plain text: only the final agent response for each step,
    /// without section headings.  Matches the legacy pre-1.0 behaviour.
    Compact,
}

// ── JSON output types ─────────────────────────────────────────────────────────

struct JsonOutput {
    title: Option<String>,
    steps: Vec<JsonStep>,
}

struct JsonStep {
    index: usize,
    label: Option<String>,
    user_input: String,
    agent_response: String,
    tools_used: Vec<String>,
    duration_ms: u64,
    success: bool,
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

        // ── Merge template vars (CI env < frontmatter < CLI) ────────────────
        // CI environment variables are injected at the lowest priority so
        // workflows can reference {{branch}}, {{commit}}, {{GITHUB_SHA}}, etc.
        // without any explicit --var flag.
        let ci_ctx = crate::context::detect_ci_context();
        let mut vars: HashMap<String, String> = crate::context::ci_template_vars(&ci_ctx);
        vars.extend(frontmatter.vars.unwrap_or_default());
        vars.extend(opts.vars.clone());

        // ── Resolve title ────────────────────────────────────────────────────
        let title = frontmatter.title
            .or_else(|| extract_h1_title(markdown_body));

        // ── Build step queue ─────────────────────────────────────────────────
        let mut queue: StepQueue = if opts.input.trim().is_empty() {
            let content = opts.extra_prompt.clone().unwrap_or_default();
            StepQueue::from(vec![Step {
                label: None,
                content,
                options: Default::default(),
            }])
        } else {
            let mut q = parse_markdown_steps(markdown_body);
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
        };

        let total = queue.len();

        // ── Dry-run mode ─────────────────────────────────────────────────────
        if opts.dry_run {
            write_progress(&format!("[sven:dry-run] Workflow validated — {} step(s)", total));
            if let Some(t) = &title {
                write_progress(&format!("[sven:dry-run] Title: {}", t));
            }
            let mut i = 0;
            while let Some(step) = queue.pop() {
                i += 1;
                let label = step.label.as_deref().unwrap_or("(unlabelled)");
                let mode_hint = step.options.mode.as_deref().unwrap_or("(inherit)");
                let timeout_hint = step.options.timeout_secs
                    .map(|t| format!("{t}s"))
                    .unwrap_or_else(|| "(inherit)".to_string());
                write_progress(&format!(
                    "[sven:dry-run] Step {i}/{total}: label={label:?} mode={mode_hint} timeout={timeout_hint}"
                ));
            }
            return Ok(());
        }

        // ── Build model config ───────────────────────────────────────────────
        let mut model_cfg = self.config.model.clone();
        let model_override = opts.model_override
            .clone()
            .or(frontmatter.model.clone());
        if let Some(name) = &model_override {
            const PROVIDER_KEYWORDS: &[&str] = &["mock", "openai", "anthropic"];
            if let Some((provider, model)) = name.split_once('/') {
                model_cfg.provider = provider.to_string();
                model_cfg.name = model.to_string();
            } else if PROVIDER_KEYWORDS.contains(&name.as_str()) {
                model_cfg.provider = name.clone();
            } else {
                model_cfg.name = name.clone();
            }
        }

        let model = sven_model::from_config(&model_cfg)
            .context("failed to initialise model provider")?;
        let model: Arc<dyn sven_model::ModelProvider> = Arc::from(model);

        // ── Build runtime context ─────────────────────────────────────────────
        let mut runtime_ctx = RuntimeContext {
            project_root: opts.project_root.clone(),
            git_context: opts.project_root.as_ref()
                .map(|r| sven_runtime::collect_git_context(r)),
            ci_context: Some(ci_ctx),
            project_context_file: opts.project_root.as_ref()
                .and_then(|r| sven_runtime::load_project_context_file(r)),
            append_system_prompt: opts.append_system_prompt.clone(),
            system_prompt_override: None,
        };

        if runtime_ctx.project_context_file.is_some() {
            write_progress("[sven:info] Project context file loaded");
        }

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

        // Resolve timeouts (CLI > frontmatter > config)
        let run_timeout_secs = opts.run_timeout_secs
            .or(frontmatter.run_timeout_secs)
            .or_else(|| if self.config.agent.max_run_timeout_secs > 0 {
                Some(self.config.agent.max_run_timeout_secs)
            } else {
                None
            });

        let global_step_timeout_secs = opts.step_timeout_secs
            .or(frontmatter.step_timeout_secs)
            .or_else(|| if self.config.agent.max_step_timeout_secs > 0 {
                Some(self.config.agent.max_step_timeout_secs)
            } else {
                None
            });

        // Resolve mode from frontmatter or CLI
        let initial_mode = frontmatter.mode
            .as_deref()
            .and_then(parse_agent_mode)
            .unwrap_or(opts.mode);

        // ── Shared state for stateful tools ──────────────────────────────────
        // The mode lock and tool-event channel are created inside
        // AgentBuilder::build() so that SwitchModeTool and the agent loop
        // share the same instances.  Only caller-owned state lives here.
        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
        let task_depth = Arc::new(AtomicUsize::new(0));

        let profile = ToolSetProfile::Full { question_tx: None, todos, task_depth };

        let mut agent = AgentBuilder::new(self.config.clone())
            .with_runtime_context(runtime_ctx)
            .build(initial_mode, model, profile);

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

        // ── Run step loop ────────────────────────────────────────────────────
        let run_start = Instant::now();
        let mut step_idx = 0usize;
        let mut collected: Vec<Message> = Vec::new();
        let mut json_steps: Vec<JsonStep> = Vec::new();

        while let Some(step) = queue.pop() {
            step_idx += 1;
            let label = step.label.as_deref().unwrap_or("(unlabelled)");

            // Check total run timeout (between steps)
            if let Some(t) = run_timeout_secs {
                if run_start.elapsed() > Duration::from_secs(t) {
                    write_stderr(&format!(
                        "[sven:error] Total run timeout exceeded ({}s). Completed {}/{} steps.",
                        t, step_idx - 1, total
                    ));
                    std::process::exit(EXIT_TIMEOUT);
                }
            }

            // Apply per-step mode override
            if let Some(mode_str) = &step.options.mode {
                if let Some(mode) = parse_agent_mode(mode_str) {
                    agent.set_mode(mode).await;
                } else {
                    write_stderr(&format!(
                        "[sven:warn] Unknown mode {:?} in step {step_idx}, continuing with current mode",
                        mode_str
                    ));
                }
            }

            // Resolve step timeout
            let step_timeout_secs = step.options.timeout_secs
                .or(global_step_timeout_secs);

            write_progress(&format!(
                "[sven:step:start] {}/{} label=\"{}\"",
                step_idx, total, label
            ));

            let step_start = Instant::now();

            // Apply variable substitution
            let step_content = if !vars.is_empty() {
                apply_template(&step.content, &vars)
            } else {
                step.content.clone()
            };

            // Mark where this step's messages begin in `collected`
            let step_msg_start = collected.len();

            // Record the user turn before submitting
            collected.push(Message::user(&step_content));

            let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
            let submit_fut = agent.submit(&step_content, tx);

            let mut response_text = String::new();
            let mut tools_used: Vec<String> = Vec::new();
            let mut failed = false;

            tokio::pin!(submit_fut);

            // Build a step-level timeout future
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
                            // Save partial conversation before aborting
                            if !collected.is_empty() {
                                let _ = history::save(&collected);
                            }
                            std::process::exit(EXIT_TIMEOUT);
                        }
                    }

                    _ = cancel_rx.recv() => {
                        write_stderr("[sven:interrupted] Ctrl+C received — saving partial conversation");
                        if !collected.is_empty() {
                            let _ = history::save(&collected);
                        }
                        std::process::exit(EXIT_INTERRUPT);
                    }

                    Some(event) = rx.recv() => {
                        handle_event(
                            event,
                            &mut response_text,
                            &mut tools_used,
                            &mut failed,
                            &mut collected,
                        );
                    }

                    result = &mut submit_fut => {
                        if let Err(e) = result {
                            write_stderr(&format!(
                                "[sven:fatal] Step {step_idx} ({label:?}) failed: {e}"
                            ));
                            std::process::exit(EXIT_AGENT_ERROR);
                        }
                        while let Ok(ev) = rx.try_recv() {
                            handle_event(
                                ev,
                                &mut response_text,
                                &mut tools_used,
                                &mut failed,
                                &mut collected,
                            );
                        }
                        break;
                    }
                }
            }

            let step_duration_ms = step_start.elapsed().as_millis() as u64;

            // ── Write step output to stdout ──────────────────────────────────
            match opts.output_format {
                OutputFormat::Conversation => {
                    let turn = &collected[step_msg_start..];
                    let md = serialize_conversation_turn(turn);
                    write_stdout(&md);
                }
                OutputFormat::Compact => {
                    if !response_text.ends_with('\n') {
                        write_stdout(&format!("{response_text}\n"));
                    } else {
                        write_stdout(&response_text);
                    }
                }
                OutputFormat::Json => {
                    // Accumulate; write at the end
                    json_steps.push(JsonStep {
                        index: step_idx,
                        label: step.label.clone(),
                        user_input: step_content.clone(),
                        agent_response: response_text.clone(),
                        tools_used: tools_used.clone(),
                        duration_ms: step_duration_ms,
                        success: !failed,
                    });
                }
            }

            // ── Write per-step artifact ──────────────────────────────────────
            if let Some(dir) = &opts.artifacts_dir {
                write_step_artifact(dir, step_idx, label, &collected[step_msg_start..]);
            }

            // ── Progress report ──────────────────────────────────────────────
            write_progress(&format!(
                "[sven:step:complete] {}/{} label=\"{}\" duration_ms={} tools={} success={}",
                step_idx, total, label, step_duration_ms, tools_used.len(), !failed
            ));

            if failed {
                write_stderr(&format!(
                    "[sven:error] Step {step_idx} ({label:?}) reported an error. Aborting."
                ));
                // Save partial conversation
                if !collected.is_empty() {
                    let _ = history::save(&collected);
                }
                std::process::exit(EXIT_AGENT_ERROR);
            }

            if step_idx < total {
                write_stderr(&format!("\n--- step {}/{} complete ---\n", step_idx, total));
            }
        }

        // ── Finalize JSON output ─────────────────────────────────────────────
        if opts.output_format == OutputFormat::Json {
            let out = JsonOutput { title, steps: json_steps };
            let json = json_output_to_string(&out);
            write_stdout(&format!("{json}\n"));
        }

        // ── --output-last-message ─────────────────────────────────────────────
        if let Some(out_path) = &opts.output_last_message {
            // Extract the last assistant response from the collected messages.
            let last_response = collected.iter().rev()
                .find(|m| m.role == Role::Assistant)
                .and_then(|m| match &m.content {
                    MessageContent::Text(t) => Some(t.clone()),
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

        // ── Persist conversation to history ──────────────────────────────────
        if !collected.is_empty() {
            if let Err(e) = history::save(&collected) {
                debug!("failed to save conversation to history: {e}");
            }
        }

        Ok(())
    }
}

// ── Event handler ─────────────────────────────────────────────────────────────

/// Process a single agent event: write diagnostics to stderr, collect
/// messages into `collected`, and track response text / tool usage.
fn handle_event(
    event: AgentEvent,
    response_text: &mut String,
    tools_used: &mut Vec<String>,
    failed: &mut bool,
    collected: &mut Vec<Message>,
) {
    match event {
        AgentEvent::TextDelta(delta) => {
            // Buffer for compact / JSON formats; conversation format emits
            // the full serialized turn after the step completes.
            response_text.push_str(&delta);
        }
        AgentEvent::TextComplete(text) => {
            if !text.is_empty() {
                collected.push(Message::assistant(&text));
            }
        }
        AgentEvent::ToolCallStarted(tc) => {
            write_stderr(&format!(
                "[sven:tool:call] name=\"{}\" args={}",
                tc.name,
                serde_json::to_string(&tc.args).unwrap_or_default()
            ));
            tools_used.push(tc.name.clone());
            let args_str = serde_json::to_string(&tc.args).unwrap_or_default();
            collected.push(Message {
                role: Role::Assistant,
                content: MessageContent::ToolCall {
                    tool_call_id: tc.id.clone(),
                    function: FunctionCall { name: tc.name.clone(), arguments: args_str },
                },
            });
        }
        AgentEvent::ToolCallFinished { call_id, tool_name, is_error, output } => {
            if is_error {
                write_stderr(&format!("[sven:tool:error] name=\"{tool_name}\" output={output:?}"));
            } else {
                write_stderr(&format!("[sven:tool:ok] name=\"{tool_name}\""));
            }
            collected.push(Message::tool_result(&call_id, &output));
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
            write_stderr(&format!(
                "[sven:questions] {}",
                questions.join(" | ")
            ));
        }
        AgentEvent::TurnComplete
        | AgentEvent::TokenUsage { .. }
        | AgentEvent::QuestionAnswer { .. }
        | AgentEvent::ThinkingDelta(_)
        | AgentEvent::ThinkingComplete(_) => {}
    }
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

fn write_step_artifact(dir: &std::path::Path, idx: usize, label: &str, messages: &[Message]) {
    let safe_label = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    let filename = format!("{:02}-{}.md", idx, safe_label);
    let path = dir.join(&filename);

    let content = serialize_conversation_turn(messages);
    if let Err(e) = std::fs::write(&path, &content) {
        write_stderr(&format!("[sven:warn] Could not write step artifact {}: {e}", path.display()));
    }
}

fn write_conversation_artifact(dir: &std::path::Path, messages: &[Message]) {
    let path = dir.join("conversation.md");
    let content = serialize_conversation_turn(messages);
    if let Err(e) = std::fs::write(&path, &content) {
        write_stderr(&format!("[sven:warn] Could not write conversation artifact: {e}"));
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn json_output_to_string(out: &JsonOutput) -> String {
    let steps: Vec<serde_json::Value> = out.steps.iter().map(|s| {
        serde_json::json!({
            "index": s.index,
            "label": s.label,
            "user_input": s.user_input,
            "agent_response": s.agent_response,
            "tools_used": s.tools_used,
            "duration_ms": s.duration_ms,
            "success": s.success,
        })
    }).collect();

    let obj = serde_json::json!({
        "title": out.title,
        "steps": steps,
    });

    serde_json::to_string_pretty(&obj).unwrap_or_else(|e| {
        format!("{{\"error\": \"serialization failed: {e}\"}}")
    })
}

fn parse_agent_mode(s: &str) -> Option<AgentMode> {
    match s.trim() {
        "research" => Some(AgentMode::Research),
        "plan" => Some(AgentMode::Plan),
        "agent" => Some(AgentMode::Agent),
        _ => None,
    }
}
