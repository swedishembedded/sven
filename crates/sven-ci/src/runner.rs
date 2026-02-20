use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use anyhow::Context;
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent, TaskTool};
use sven_input::{parse_markdown_steps, Step, StepQueue};
use sven_tools::{
    events::{TodoItem, ToolEvent},
    AskQuestionTool, ApplyPatchTool, DeleteFileTool, EditFileTool,
    GlobFileSearchTool, GrepTool, ListDirTool, ReadFileTool, ReadLintsTool,
    RunTerminalCommandTool, SearchCodebaseTool, SwitchModeTool, TodoWriteTool,
    UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
    ToolRegistry,
};

use crate::output::{finalise_stdout, write_stderr, write_stdout};

/// Options for the CI runner.
#[derive(Debug)]
pub struct CiOptions {
    pub mode: AgentMode,
    pub model_override: Option<String>,
    /// The markdown input to process.  May come from a file or stdin.
    pub input: String,
    /// Extra prompt appended before the first step (from positional CLI args)
    pub extra_prompt: Option<String>,
}

/// Headless CI runner that processes a [`StepQueue`] sequentially and writes
/// clean text to stdout.
pub struct CiRunner {
    config: Arc<Config>,
}

impl CiRunner {
    pub fn new(config: Arc<Config>) -> Self {
        Self { config }
    }

    pub async fn run(&self, opts: CiOptions) -> anyhow::Result<()> {
        let mut model_cfg = self.config.model.clone();
        if let Some(name) = &opts.model_override {
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

        let agent_cfg = Arc::new(self.config.agent.clone());

        // Shared state for stateful tools
        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
        let current_mode: Arc<Mutex<AgentMode>> = Arc::new(Mutex::new(opts.mode));
        let (tool_event_tx, _tool_event_rx) = mpsc::channel::<ToolEvent>(64);
        let task_depth = Arc::new(AtomicUsize::new(0));

        let tools = Arc::new(build_registry(
            &self.config,
            todos,
            current_mode,
            tool_event_tx,
            model.clone(),
            agent_cfg.clone(),
            task_depth,
        ));

        let mut agent = Agent::new(
            model,
            tools,
            agent_cfg,
            opts.mode,
            128_000,
        );

        // Build the step queue from input markdown
        let mut queue: StepQueue = if opts.input.trim().is_empty() {
            let content = opts.extra_prompt.clone().unwrap_or_default();
            StepQueue::from(vec![sven_input::Step { label: None, content }])
        } else {
            let mut q = parse_markdown_steps(&opts.input);
            if let Some(prompt) = &opts.extra_prompt {
                let mut prepended = StepQueue::from(vec![Step {
                    label: None,
                    content: prompt.clone(),
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
        let mut step_idx = 0usize;

        while let Some(step) = queue.pop() {
            step_idx += 1;
            let label = step.label.as_deref().unwrap_or("(unlabelled)");
            debug!(step = step_idx, total, label, "processing CI step");

            let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);
            let submit_fut = agent.submit(&step.content, tx);

            let mut response_text = String::new();
            let mut failed = false;

            tokio::pin!(submit_fut);

            loop {
                tokio::select! {
                    biased;

                    Some(event) = rx.recv() => {
                        handle_event(event, &mut response_text, &mut failed);
                    }

                    result = &mut submit_fut => {
                        if let Err(e) = result {
                            write_stderr(&format!("[fatal] {e}"));
                            std::process::exit(1);
                        }
                        while let Ok(ev) = rx.try_recv() {
                            handle_event(ev, &mut response_text, &mut failed);
                        }
                        break;
                    }
                }
            }

            finalise_stdout(&response_text);

            if failed {
                std::process::exit(1);
            }

            if step_idx < total {
                write_stderr(&format!("\n--- step {}/{} complete ---\n", step_idx, total));
            }
        }

        Ok(())
    }
}

fn handle_event(event: AgentEvent, response_text: &mut String, failed: &mut bool) {
    match event {
        AgentEvent::TextDelta(delta) => {
            write_stdout(&delta);
            response_text.push_str(&delta);
        }
        AgentEvent::ToolCallStarted(tc) => {
            write_stderr(&format!(
                "[tool] {} ({})",
                tc.name,
                serde_json::to_string(&tc.args).unwrap_or_default()
            ));
        }
        AgentEvent::ToolCallFinished { tool_name, is_error, output, .. } => {
            if is_error {
                write_stderr(&format!("[tool error] {tool_name}: {output}"));
            } else {
                write_stderr(&format!("[tool ok] {tool_name}"));
            }
        }
        AgentEvent::ContextCompacted { tokens_before, tokens_after } => {
            write_stderr(&format!(
                "[compacted context: {} → {} tokens]",
                tokens_before, tokens_after
            ));
        }
        AgentEvent::Error(msg) => {
            write_stderr(&format!("[agent error] {msg}"));
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
            write_stderr(&format!("[todos]\n{}", lines.join("\n")));
        }
        AgentEvent::ModeChanged(mode) => {
            write_stderr(&format!("[mode changed] now in {mode} mode"));
        }
        AgentEvent::Question { questions, .. } => {
            write_stderr(&format!("[questions] {}", questions.join(" | ")));
        }
        AgentEvent::TurnComplete | AgentEvent::TextComplete(_) |
        AgentEvent::TokenUsage { .. } | AgentEvent::QuestionAnswer { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn build_registry(
    cfg: &Config,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    current_mode: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    model: Arc<dyn sven_model::ModelProvider>,
    agent_cfg: Arc<sven_config::AgentConfig>,
    task_depth: Arc<AtomicUsize>,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // Read-only / all-mode tools
    reg.register(ReadFileTool);
    reg.register(ListDirTool);
    reg.register(GlobFileSearchTool);
    reg.register(GrepTool);
    reg.register(SearchCodebaseTool);
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });
    reg.register(ReadLintsTool);
    reg.register(UpdateMemoryTool {
        memory_file: cfg.tools.memory.memory_file.clone(),
    });
    reg.register(AskQuestionTool::new());

    // Stateful tools
    reg.register(TodoWriteTool::new(todos, tool_event_tx.clone()));
    reg.register(SwitchModeTool::new(current_mode, tool_event_tx));

    // Agent-mode write tools
    reg.register(WriteTool);
    reg.register(EditFileTool);
    reg.register(DeleteFileTool);
    reg.register(ApplyPatchTool);
    reg.register(RunTerminalCommandTool {
        timeout_secs: cfg.tools.timeout_secs,
    });

    // Sub-agent spawner
    reg.register(TaskTool::new(
        model,
        Arc::new(cfg.clone()),
        agent_cfg,
        task_depth,
    ));

    reg
}
