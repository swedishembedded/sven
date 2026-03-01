// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TaskTool — spawns a sub-agent to complete a focused sub-task.
//!
//! Moved from `sven-core` to `sven-bootstrap` so that TaskTool can use
//! `build_tool_registry` without creating a circular dependency
//! (sven-core → sven-tools, sven-bootstrap → sven-core + sven-tools).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::{AgentConfig, AgentMode, Config};
use sven_core::{Agent, AgentEvent, AgentRuntimeContext};

use sven_tools::{
    events::{TodoItem, ToolEvent},
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
};

use crate::context::ToolSetProfile;
use crate::registry::build_tool_registry;

const MAX_DEPTH: usize = 3;

/// Spawns a sub-agent to complete a focused task and returns its text output.
pub struct TaskTool {
    model: Arc<dyn sven_model::ModelProvider>,
    config: Arc<Config>,
    depth: Arc<AtomicUsize>,
    /// Runtime context to pass to each sub-agent (project root, CI/git notes,
    /// AGENTS.md content).  Sub-agents inherit the parent's context so they
    /// know where to operate.
    sub_agent_runtime: AgentRuntimeContext,
}

impl TaskTool {
    pub fn new(
        model: Arc<dyn sven_model::ModelProvider>,
        config: Arc<Config>,
        depth: Arc<AtomicUsize>,
        sub_agent_runtime: AgentRuntimeContext,
    ) -> Self {
        Self {
            model,
            config,
            depth,
            sub_agent_runtime,
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

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
            "required": ["prompt"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    fn modes(&self) -> &[AgentMode] {
        &[AgentMode::Agent]
    }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'prompt'"),
        };
        let mode_str = call
            .args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("agent");
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

        let mut sub_config: AgentConfig = self.config.agent.clone();
        if let Some(max_rounds) = call.args.get("max_rounds").and_then(|v| v.as_u64()) {
            sub_config.max_tool_rounds = max_rounds as u32;
        }

        let todos: Arc<Mutex<Vec<TodoItem>>> = Arc::new(Mutex::new(Vec::new()));

        let profile = ToolSetProfile::SubAgent { todos };

        // Shared mode lock and tool-event channel wired through to the agent
        // so SwitchModeTool and TodoWriteTool events are correctly observed.
        let mode_lock = Arc::new(Mutex::new(mode));
        let (tool_event_tx, tool_event_rx) = mpsc::channel::<ToolEvent>(64);

        // Sub-agents use SubAgent profile (no TaskTool), so sub_agent_runtime
        // is unused — pass default.
        let tools = Arc::new(build_tool_registry(
            &self.config,
            self.model.clone(),
            profile,
            mode_lock.clone(),
            tool_event_tx,
            AgentRuntimeContext::default(),
        ));

        let mut agent = Agent::new(
            self.model.clone(),
            tools,
            Arc::new(sub_config),
            self.sub_agent_runtime.clone(),
            mode_lock,
            tool_event_rx,
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
