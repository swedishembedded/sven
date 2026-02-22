// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: MIT
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tracing::debug;

use sven_config::AgentMode;

use crate::events::ToolEvent;
use crate::policy::ApprovalPolicy;
use crate::tool::{Tool, ToolCall, ToolOutput};

pub struct SwitchModeTool {
    current_mode: Arc<Mutex<AgentMode>>,
    event_tx: mpsc::Sender<ToolEvent>,
}

impl SwitchModeTool {
    pub fn new(current_mode: Arc<Mutex<AgentMode>>, event_tx: mpsc::Sender<ToolEvent>) -> Self {
        Self { current_mode, event_tx }
    }
}

#[async_trait]
impl Tool for SwitchModeTool {
    fn name(&self) -> &str { "switch_mode" }

    fn description(&self) -> &str {
        "Switch the agent's operating mode to match the current task type.\n\n\
         ## Modes\n\
         - 'agent': Make code changes, write files, run commands\n\
         - 'plan': Design approaches, create structured plans, no writes\n\
         - 'research': Explore and learn, read-only, no modifications\n\n\
         ## Mode Capabilities\n\
         Agent: Read/write files, execute commands, modify code\n\
         Plan: Read files, design approaches, output markdown plans\n\
         Research: Read files, search code, gather information\n\n\
         ## When to Switch\n\
         - Research → Plan: Task requires planning before implementation\n\
         - Plan → Agent: Ready to implement the plan\n\
         - Agent → Plan: Need to step back and design approach\n\
         - Agent → Research: Need to explore before proceeding\n\n\
         ## Examples\n\
         <example>\n\
         Start research phase:\n\
         switch_mode: mode=\"research\"\n\
         </example>\n\
         <example>\n\
         Move to planning:\n\
         switch_mode: mode=\"plan\"\n\
         </example>\n\
         <example>\n\
         Begin implementation:\n\
         switch_mode: mode=\"agent\"\n\
         </example>\n\n\
         ## IMPORTANT\n\
         - Can only downgrade: agent → plan → research\n\
         - Upgrading requires user request\n\
         - Current mode determines available tools\n\
         - Switch proactively when task type changes\n\
         - Use plan mode for complex tasks before coding"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["research", "plan", "agent"],
                    "description": "Target mode to switch to"
                }
            },
            "required": ["mode"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy { ApprovalPolicy::Auto }

    fn modes(&self) -> &[AgentMode] { &[AgentMode::Agent, AgentMode::Plan] }

    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        let mode_str = match call.args.get("mode").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return ToolOutput::err(&call.id, "missing 'mode'"),
        };

        let target = match mode_str.as_str() {
            "research" => AgentMode::Research,
            "plan" => AgentMode::Plan,
            "agent" => AgentMode::Agent,
            other => return ToolOutput::err(&call.id, format!("unknown mode: {other}")),
        };

        let current = *self.current_mode.lock().await;

        debug!(from = ?current, to = ?target, "switch_mode tool");

        // Only allow downgrading
        let is_downgrade = match (current, target) {
            (AgentMode::Agent, AgentMode::Plan) => true,
            (AgentMode::Agent, AgentMode::Research) => true,
            (AgentMode::Plan, AgentMode::Research) => true,
            (from, to) if from == to => {
                return ToolOutput::ok(&call.id, format!("already in {mode_str} mode"));
            }
            _ => false,
        };

        if !is_downgrade {
            return ToolOutput::err(
                &call.id,
                format!(
                    "cannot switch from {current} to {target}: upgrading modes is not allowed"
                ),
            );
        }

        *self.current_mode.lock().await = target;
        let _ = self.event_tx.send(ToolEvent::ModeChanged(target)).await;

        ToolOutput::ok(&call.id, format!("switched to {target} mode"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    use crate::tool::{Tool, ToolCall};

    fn make_tool(mode: AgentMode) -> (SwitchModeTool, Arc<Mutex<AgentMode>>, mpsc::Receiver<ToolEvent>) {
        let current = Arc::new(Mutex::new(mode));
        let (tx, rx) = mpsc::channel(16);
        let tool = SwitchModeTool::new(current.clone(), tx);
        (tool, current, rx)
    }

    fn call(mode: &str) -> ToolCall {
        ToolCall { id: "s1".into(), name: "switch_mode".into(), args: json!({"mode": mode}) }
    }

    #[tokio::test]
    async fn agent_can_downgrade_to_plan() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&call("plan")).await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(*current.lock().await, AgentMode::Plan);
    }

    #[tokio::test]
    async fn agent_can_downgrade_to_research() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&call("research")).await;
        assert!(!out.is_error);
        assert_eq!(*current.lock().await, AgentMode::Research);
    }

    #[tokio::test]
    async fn research_cannot_upgrade_to_agent() {
        let (tool, _current, _rx) = make_tool(AgentMode::Research);
        let out = tool.execute(&call("agent")).await;
        assert!(out.is_error);
        assert!(out.content.contains("not allowed"));
    }

    #[tokio::test]
    async fn plan_cannot_upgrade_to_agent() {
        let (tool, _current, _rx) = make_tool(AgentMode::Plan);
        let out = tool.execute(&call("agent")).await;
        assert!(out.is_error);
        assert!(out.content.contains("not allowed"));
    }

    #[tokio::test]
    async fn same_mode_is_noop() {
        let (tool, current, _rx) = make_tool(AgentMode::Agent);
        let out = tool.execute(&call("agent")).await;
        assert!(!out.is_error);
        assert!(out.content.contains("already in"));
        assert_eq!(*current.lock().await, AgentMode::Agent);
    }

    #[tokio::test]
    async fn emits_mode_changed_event() {
        let (tool, _current, mut rx) = make_tool(AgentMode::Agent);
        tool.execute(&call("plan")).await;
        let event = rx.try_recv().expect("should emit event");
        matches!(event, ToolEvent::ModeChanged(AgentMode::Plan));
    }

    #[tokio::test]
    async fn missing_mode_is_error() {
        let (tool, _current, _rx) = make_tool(AgentMode::Agent);
        let call = ToolCall { id: "1".into(), name: "switch_mode".into(), args: json!({}) };
        let out = tool.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing 'mode'"));
    }
}
