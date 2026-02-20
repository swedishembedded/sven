use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentConfig, AgentMode, Config};
use sven_tools::{
    events::{TodoItem, ToolEvent},
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
    AskQuestionTool, ApplyPatchTool, DeleteFileTool, EditFileTool,
    GlobFileSearchTool, GrepTool, ListDirTool, ReadFileTool, ReadLintsTool,
    RunTerminalCommandTool, SearchCodebaseTool, SwitchModeTool, TodoWriteTool,
    UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
    ToolRegistry,
};

use crate::agent::Agent;
use crate::events::AgentEvent;

const MAX_DEPTH: usize = 3;

pub struct TaskTool {
    model: Arc<dyn sven_model::ModelProvider>,
    config: Arc<Config>,
    agent_config: Arc<AgentConfig>,
    depth: Arc<AtomicUsize>,
}

impl TaskTool {
    pub fn new(
        model: Arc<dyn sven_model::ModelProvider>,
        config: Arc<Config>,
        agent_config: Arc<AgentConfig>,
        depth: Arc<AtomicUsize>,
    ) -> Self {
        Self { model, config, agent_config, depth }
    }

    fn build_sub_registry(&self) -> ToolRegistry {
        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));
        let mode: Arc<Mutex<AgentMode>> = Arc::new(Mutex::new(AgentMode::Agent));
        let (tx, _rx) = mpsc::channel::<ToolEvent>(64);

        let mut reg = ToolRegistry::new();
        reg.register(ReadFileTool);
        reg.register(ListDirTool);
        reg.register(GlobFileSearchTool);
        reg.register(GrepTool);
        reg.register(SearchCodebaseTool);
        reg.register(ReadLintsTool);
        reg.register(AskQuestionTool::new());
        reg.register(WebFetchTool);
        reg.register(WebSearchTool {
            api_key: self.config.tools.web.search.api_key.clone(),
        });
        reg.register(UpdateMemoryTool {
            memory_file: self.config.tools.memory.memory_file.clone(),
        });
        reg.register(TodoWriteTool::new(todos, tx.clone()));
        reg.register(SwitchModeTool::new(mode, tx.clone()));
        reg.register(WriteTool);
        reg.register(EditFileTool);
        reg.register(DeleteFileTool);
        reg.register(ApplyPatchTool);
        reg.register(RunTerminalCommandTool {
            timeout_secs: self.config.tools.timeout_secs,
        });
        // Note: TaskTool is intentionally NOT registered here to limit nesting
        reg
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }

    fn description(&self) -> &str {
        "Spawn a sub-agent to complete a focused task and return its final text output. \
         Useful for delegating isolated sub-tasks. The sub-agent has access to all standard \
         tools. Maximum nesting depth is 3."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task description for the sub-agent"
                },
                "mode": {
                    "type": "string",
                    "enum": ["research", "plan", "agent"],
                    "description": "Operating mode for the sub-agent (default: agent)"
                },
                "max_rounds": {
                    "type": "integer",
                    "description": "Maximum tool-call rounds (default: from config)"
                }
            },
            "required": ["prompt"]
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Ask }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'prompt'"),
        };
        let mode_str = call.args.get("mode").and_then(|v| v.as_str()).unwrap_or("agent");
        let mode = match mode_str {
            "research" => AgentMode::Research,
            "plan" => AgentMode::Plan,
            "agent" => AgentMode::Agent,
            other => return ToolOutput::err(&call.id, format!("unknown mode: {other}")),
        };

        let current_depth = self.depth.load(Ordering::Relaxed);
        if current_depth >= MAX_DEPTH {
            return ToolOutput::err(
                &call.id,
                format!("maximum sub-agent depth ({MAX_DEPTH}) reached"),
            );
        }

        self.depth.fetch_add(1, Ordering::Relaxed);
        debug!(prompt = %prompt, mode = %mode, depth = current_depth + 1, "task: spawning sub-agent");

        let mut sub_config = (*self.agent_config).clone();
        if let Some(max_rounds) = call.args.get("max_rounds").and_then(|v| v.as_u64()) {
            sub_config.max_tool_rounds = max_rounds as u32;
        }

        let tools = Arc::new(self.build_sub_registry());

        let mut agent = Agent::new(
            self.model.clone(),
            tools,
            Arc::new(sub_config),
            mode,
            128_000,
        );

        let (tx, mut rx) = mpsc::channel::<AgentEvent>(256);

        let submit_result = agent.submit(&prompt, tx).await;

        let mut output = String::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::TextDelta(delta) = event {
                output.push_str(&delta);
            }
        }

        self.depth.fetch_sub(1, Ordering::Relaxed);

        match submit_result {
            Ok(_) => {
                if output.is_empty() {
                    ToolOutput::ok(&call.id, "(sub-agent produced no text output)")
                } else {
                    ToolOutput::ok(&call.id, output)
                }
            }
            Err(e) => ToolOutput::err(&call.id, format!("sub-agent error: {e}")),
        }
    }
}
