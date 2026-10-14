// Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//
// Swedish Embedded AB implements solutions for delegating work to LLM
// sub-agents for its clients. If your team needs expertise in agent
// delegation then you can procure our services by sending an email to
// info@swedishembedded.com.
//! The `task` actions that do not spawn: reading a finished sub-agent's output
//! buffer (`status`, `read`, `grep`) and listing the discovered subagents
//! (`agents`) and what the finished ones cost (`ledger`).

use serde_json::{json, Value};
use sven_tool_api::{ToolCall, ToolOutput};
use sven_tools_fs::{BufGrepTool, BufReadTool, BufStatusTool};
use sven_workspace::AgentInfo;

use super::TaskTool;
use sven_tool_api::Tool as _;

/// The string argument `key` of `call`, if present and not empty.
fn text<'a>(call: &'a ToolCall, key: &str) -> Option<&'a str> {
    call.args
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
}

/// The number argument `key` of `call`, or `default`.
fn number(call: &ToolCall, key: &str, default: u64) -> u64 {
    call.args
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or(default)
}

/// One subagent as `agents` shows it: its name, flags and whole description.
fn describe(agent: &AgentInfo) -> String {
    let mut flags = Vec::new();
    if agent.readonly {
        flags.push("(readonly)".to_string());
    }
    if let Some(model) = &agent.model {
        flags.push(format!("model: {model}"));
    }
    let flags = if flags.is_empty() {
        String::new()
    } else {
        format!(" {}", flags.join(", "))
    };
    format!("- {}{flags}: {}", agent.name, agent.description.trim())
}

impl TaskTool {
    /// Runs `action` when it is not a spawn, or returns `None` for a spawn.
    pub(super) async fn inspect(&self, action: &str, call: &ToolCall) -> Option<ToolOutput> {
        let handle = |action: &str| {
            text(call, "handle").ok_or_else(|| {
                ToolOutput::err(
                    &call.id,
                    format!("missing required parameter 'handle' for action={action}"),
                )
            })
        };
        let forward = |name: &str, args: Value| ToolCall {
            id: call.id.clone(),
            name: name.into(),
            args,
        };
        let store = self.buffer_store.clone();
        Some(match action {
            "status" => match handle(action) {
                Ok(h) => {
                    BufStatusTool::new(store)
                        .execute(&forward("buf_status", json!({ "handle": h })))
                        .await
                }
                Err(refusal) => refusal,
            },
            "read" => match handle(action) {
                Ok(h) => {
                    let args = json!({
                        "handle": h,
                        "start_line": number(call, "start_line", 1),
                        "end_line": number(call, "end_line", 50),
                    });
                    BufReadTool::new(store)
                        .execute(&forward("buf_read", args))
                        .await
                }
                Err(refusal) => refusal,
            },
            "grep" => match (handle(action), text(call, "pattern")) {
                (Ok(h), Some(pattern)) => {
                    let args = json!({
                        "handle": h,
                        "pattern": pattern,
                        "context_lines": number(call, "context_lines", 2),
                        "limit": number(call, "limit", 50),
                    });
                    BufGrepTool::new(store)
                        .execute(&forward("buf_grep", args))
                        .await
                }
                (Err(refusal), _) => refusal,
                (Ok(_), None) => ToolOutput::err(
                    &call.id,
                    "missing required parameter 'pattern' for action=grep",
                ),
            },
            "agents" => {
                let agents = self.agents.get();
                if agents.is_empty() {
                    ToolOutput::ok(&call.id, "(no subagents are defined)")
                } else {
                    let lines: Vec<String> = agents.iter().map(describe).collect();
                    ToolOutput::ok(&call.id, lines.join("\n"))
                }
            }
            "ledger" => ToolOutput::ok(&call.id, self.ledger.render()),
            _ => return None,
        })
    }
}
