use std::sync::Arc;

use anyhow::Context;
use tokio::sync::mpsc;
use tracing::debug;

use sven_config::{AgentMode, Config};
use sven_core::{Agent, AgentEvent};
use sven_input::{parse_markdown_steps, Step, StepQueue};
use sven_tools::{FsTool, GlobTool, ShellTool, ToolRegistry};

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
            // Accept three forms:
            //   "provider/model"  → sets both fields (e.g. "anthropic/claude-3-5")
            //   bare provider keyword → sets provider only (e.g. "mock", "openai")
            //   bare model name   → sets model name only (e.g. "gpt-4o")
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

        let tools = build_registry(&self.config);
        let agent_cfg = Arc::new(self.config.agent.clone());

        let mut agent = Agent::new(
            model,
            Arc::new(tools),
            agent_cfg,
            opts.mode,
            128_000,
        );

        // Build the step queue from input markdown
        let mut queue: StepQueue = if opts.input.trim().is_empty() {
            // Nothing from stdin/file — use extra_prompt as a single step
            let content = opts.extra_prompt.clone().unwrap_or_default();
            StepQueue::from(vec![sven_input::Step { label: None, content }])
        } else {
            let mut q = parse_markdown_steps(&opts.input);
            // If extra_prompt given, prepend it as step 0
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

            // Collect full text as we stream
            let mut response_text = String::new();
            let mut failed = false;

            tokio::pin!(submit_fut);

            loop {
                tokio::select! {
                    // Prefer the event channel so we never miss a TextDelta or
                    // TurnComplete when the submit future resolves at the same
                    // time as buffered events become available.
                    biased;

                    Some(event) = rx.recv() => {
                        match event {
                            AgentEvent::TextDelta(delta) => {
                                write_stdout(&delta);
                                response_text.push_str(&delta);
                            }
                            AgentEvent::ToolCallStarted(tc) => {
                                write_stderr(&format!("[tool] {} ({})", tc.name, serde_json::to_string(&tc.args).unwrap_or_default()));
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
                                failed = true;
                            }
                            AgentEvent::TurnComplete => break,
                            _ => {}
                        }
                    }

                    result = &mut submit_fut => {
                        if let Err(e) = result {
                            write_stderr(&format!("[fatal] {e}"));
                            // Propagate exit code 1 to abort pipelines with set -e
                            std::process::exit(1);
                        }
                        // submit_fut completed: by now all events have been
                        // placed into the channel.  Drain whatever remains
                        // (using try_recv so we don't block) before exiting the
                        // loop.  This closes the window between the future
                        // returning and all buffered events being consumed.
                        while let Ok(ev) = rx.try_recv() {
                            match ev {
                                AgentEvent::TextDelta(delta) => {
                                    write_stdout(&delta);
                                    response_text.push_str(&delta);
                                }
                                AgentEvent::ToolCallStarted(tc) => {
                                    write_stderr(&format!("[tool] {} ({})", tc.name, serde_json::to_string(&tc.args).unwrap_or_default()));
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
                                    failed = true;
                                }
                                AgentEvent::TurnComplete | _ => {}
                            }
                        }
                        break;
                    }
                }
            }

            finalise_stdout(&response_text);

            if failed {
                std::process::exit(1);
            }

            // Between steps add a separator on stderr (not stdout) so the
            // pipeline output stays clean
            if step_idx < total {
                write_stderr(&format!("\n--- step {}/{} complete ---\n", step_idx, total));
            }
        }

        Ok(())
    }
}

fn build_registry(cfg: &Config) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(ShellTool { timeout_secs: cfg.tools.timeout_secs });
    reg.register(FsTool);
    reg.register(GlobTool);
    reg
}
