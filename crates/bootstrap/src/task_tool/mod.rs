// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! TaskTool - spawns a full sven ACP subagent to execute a focused task.
//!
//! # Architecture
//!
//! The task tool spawns `sven acp serve` as a child process and connects to it
//! via the ACP (Agent Client Protocol) over the child's stdin/stdout pipes.
//! This gives fully structured event streaming (text deltas, tool calls, thinking
//! blocks) instead of raw text output.
//!
//! ## Event flow
//!
//! ```text
//! TaskTool                sven acp serve subprocess
//!   │                            │
//!   ├── initialize ─────────────►│
//!   ├── new_session ────────────►│
//!   ├── (set_session_mode) ─────►│
//!   ├── prompt ─────────────────►│
//!   │                            │ ── session/update notifications ──►
//!   │◄── SubagentEvent(TUI) ─────┤    (forwarded to parent TUI)
//!   │                            │
//!   │◄── PromptResponse(done) ───┤
//!   └── ToolOutput(final_text)
//! ```
//!
//! ## Inactivity timeout
//!
//! A pinned `tokio::time::Sleep` future is reset on every ACP notification.
//! If no notification arrives within [`INACTIVITY_TIMEOUT`], ACP `session/cancel`
//! is forwarded to the child and the tool returns an error.
//!
//! ## Thread model
//!
//! The ACP `ClientSideConnection` is `!Send` (it uses `LocalBoxFuture` internally
//! for spawning sub-tasks).  To keep the outer `Tool::execute` impl `Send`, the
//! entire ACP session runs in a dedicated OS thread via `std::thread::spawn` with
//! its own single-threaded tokio runtime and a `LocalSet`.
//!
//! ## Cancellation
//!
//! A [`CancelGuard`] RAII type holds the cancel sender.  When `execute` is
//! dropped (parent task cancelled) or completes normally, the guard fires the
//! sender, signalling the OS thread to forward ACP `session/cancel` to the child.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::debug;

use sven_config::AgentMode;
use sven_tools::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
};
use sven_tools_fs::{BufGrepTool, BufReadTool, BufStatusTool, BufferSource, OutputBufferStore};
use sven_workspace::{AgentInfo, SharedAgents};

mod session;
use session::{run_acp_session, CancelGuard, SpawnArgs};

/// Environment variable set when running as a subagent (depth 0).
const DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

// ── TaskTool ─────────────────────────────────────────────────────────────────

pub struct TaskTool {
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    default_model: Option<String>,
    /// Discovered subagent personas (`.sven/agents/*.md` etc).  `mode` accepts
    /// any of these names in addition to the three built-ins, giving each
    /// persona its own system prompt (persona `content`) and tool profile
    /// (`readonly` selects the ACP session mode - see [`resolve_mode_and_prompt`]).
    agents: SharedAgents,
}

impl TaskTool {
    pub fn new(
        buffer_store: Arc<Mutex<OutputBufferStore>>,
        tool_event_tx: mpsc::Sender<ToolEvent>,
        default_model: Option<String>,
        agents: SharedAgents,
    ) -> Self {
        Self {
            buffer_store,
            tool_event_tx,
            default_model,
            agents,
        }
    }
}

/// Resolve a requested `mode` against the three built-in modes and any
/// discovered subagent persona, returning the effective ACP session mode and
/// the prompt actually sent to the sub-agent.
///
/// A persona match prepends its `content` (system prompt) to `prompt` and
/// maps `readonly` to an ACP mode: `true` -> `research` (read-only tool
/// profile), `false` -> `agent` (full read/write tool profile). Built-in
/// mode names always take priority over a same-named persona.
fn resolve_mode_and_prompt(
    mode: &str,
    prompt: &str,
    agents: &[AgentInfo],
) -> Result<(String, String), String> {
    if matches!(mode, "research" | "plan" | "agent") {
        return Ok((mode.to_string(), prompt.to_string()));
    }

    if let Some(persona) = agents.iter().find(|a| a.name == mode) {
        let effective_mode = if persona.readonly { "research" } else { "agent" };
        let effective_prompt =
            format!("{}\n\n---\n\n## Task\n\n{prompt}", persona.content.trim());
        return Ok((effective_mode.to_string(), effective_prompt));
    }

    let mut valid: Vec<&str> = vec!["research", "plan", "agent"];
    valid.extend(agents.iter().map(|a| a.name.as_str()));
    Err(format!(
        "unknown mode '{mode}'. Valid options: {}",
        valid.join(", ")
    ))
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Spawn a focused sub-agent or inspect a running sub-agent's output.\n\
         action: spawn (default) | status | read | grep\n\n\
         **Spawn workflow (action=spawn or omitted):**\n\
         1. Call `task` with prompt → subagent runs and returns its final response\n\
         2. Optionally spawn more sub-agents in parallel with different prompts\n\
         3. The tool blocks until the subagent completes and returns the result\n\n\
         **When to spawn:**\n\
         - Exploration and research of large unfamiliar areas.\n\
         - Tasks that searching through a lot of context but we are only interested in the final findings. \n\
         **Modes:** `research`/`plan`/`agent` are built in. `mode` also accepts the name of any\n\
         discovered subagent persona (see the Subagents section of your system prompt) - the\n\
         persona's own instructions become the sub-agent's system prompt and its `readonly`\n\
         flag picks the tool profile.\n\n\
         **Important:**\n\
         - Do not use for anything you can easily do with shell.\n\
         - Do not spawn tasks for simple single step commands.\n\
         - Do not spawn tasks for exploring single files or anything that you can readily do directly. \n\
         Sub-agents have access to all standard tools. Sub-agents cannot spawn further sub-agents."
    }

    fn parameters_schema(&self) -> Value {
        let mut mode_enum: Vec<String> = vec!["research".into(), "plan".into(), "agent".into()];
        mode_enum.extend(self.agents.get().iter().map(|a| a.name.clone()));

        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["spawn", "status", "read", "grep"],
                    "description": "spawn (default): launch sub-agent; status/read/grep: inspect existing buffer"
                },
                "prompt": {
                    "type": "string",
                    "description": "[action=spawn] Complete, self-contained task description for the sub-agent"
                },
                "description": {
                    "type": "string",
                    "description": "[action=spawn] Short human-readable label (shown in TUI)"
                },
                "mode": {
                    "type": "string",
                    "enum": mode_enum,
                    "description": "[action=spawn] Operating mode, or the name of a discovered subagent persona (default: agent)"
                },
                "workdir": {
                    "type": "string",
                    "description": "[action=spawn] Working directory (defaults to current)"
                },
                "model": {
                    "type": "string",
                    "description": "[action=spawn] Model override (e.g. 'fast')"
                },
                "handle": {
                    "type": "string",
                    "description": "[action=status|read|grep] Buffer handle from a previous spawn"
                },
                "start_line": {
                    "type": "integer",
                    "description": "[action=read] First line to read (1-indexed)"
                },
                "end_line": {
                    "type": "integer",
                    "description": "[action=read] Last line to read (inclusive)"
                },
                "pattern": {
                    "type": "string",
                    "description": "[action=grep] Regex pattern to search for in the buffer"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "[action=grep] Lines of context before/after each match (default 2)"
                },
                "limit": {
                    "type": "integer",
                    "description": "[action=grep] Max matches (default 50)"
                }
            },
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
        let action = call
            .args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("spawn");

        // ── Buffer inspection actions ─────────────────────────────────────────
        match action {
            "status" => {
                let handle = match call
                    .args
                    .get("handle")
                    .and_then(|v| v.as_str())
                    .filter(|h| !h.is_empty())
                {
                    Some(h) => h.to_string(),
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'handle' for action=status",
                        )
                    }
                };
                return BufStatusTool::new(self.buffer_store.clone())
                    .execute(&ToolCall {
                        id: call.id.clone(),
                        name: "buf_status".into(),
                        args: serde_json::json!({ "handle": handle }),
                    })
                    .await;
            }
            "read" => {
                let handle = match call
                    .args
                    .get("handle")
                    .and_then(|v| v.as_str())
                    .filter(|h| !h.is_empty())
                {
                    Some(h) => h.to_string(),
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'handle' for action=read",
                        )
                    }
                };
                let start_line = call
                    .args
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1);
                let end_line = call
                    .args
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);
                return BufReadTool::new(self.buffer_store.clone())
                    .execute(&ToolCall {
                        id: call.id.clone(),
                        name: "buf_read".into(),
                        args: serde_json::json!({
                            "handle": handle,
                            "start_line": start_line,
                            "end_line": end_line
                        }),
                    })
                    .await;
            }
            "grep" => {
                let handle = match call
                    .args
                    .get("handle")
                    .and_then(|v| v.as_str())
                    .filter(|h| !h.is_empty())
                {
                    Some(h) => h.to_string(),
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'handle' for action=grep",
                        )
                    }
                };
                let pattern = match call.args.get("pattern").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return ToolOutput::err(
                            &call.id,
                            "missing required parameter 'pattern' for action=grep",
                        )
                    }
                };
                let context_lines = call
                    .args
                    .get("context_lines")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2);
                let limit = call
                    .args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50);
                return BufGrepTool::new(self.buffer_store.clone())
                    .execute(&ToolCall {
                        id: call.id.clone(),
                        name: "buf_grep".into(),
                        args: serde_json::json!({
                            "handle": handle,
                            "pattern": pattern,
                            "context_lines": context_lines,
                            "limit": limit
                        }),
                    })
                    .await;
            }
            _ => {}
        }

        // ── Validate spawn inputs ─────────────────────────────────────────────
        let prompt = match call.args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolOutput::err(&call.id, "missing required parameter 'prompt'"),
        };

        let description = call
            .args
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or(&prompt[..prompt.len().min(60)])
            .to_string();

        let requested_mode = call
            .args
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("agent")
            .to_string();

        let workdir = call
            .args
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));

        let model_override = call
            .args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| self.default_model.clone());

        // `acp_mode` is the built-in ACP session mode a persona maps onto;
        // `requested_mode` (kept for the TUI buffer label) may be a persona
        // name such as "knowledge-extract".
        let (acp_mode, prompt) =
            match resolve_mode_and_prompt(&requested_mode, &prompt, &self.agents.get()) {
                Ok(v) => v,
                Err(e) => return ToolOutput::err(&call.id, e),
            };

        // Subagents (DEPTH_ENV set) cannot spawn further sub-agents.
        if std::env::var(DEPTH_ENV).is_ok() {
            return ToolOutput::err(&call.id, "sub-agents cannot spawn further sub-agents");
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                return ToolOutput::err(&call.id, format!("could not locate sven executable: {e}"))
            }
        };

        // Allocate a handle ID for TUI session linking.
        let handle_id = {
            let mut store = self.buffer_store.lock().await;
            store.create(BufferSource::Subagent {
                prompt: prompt.clone(),
                mode: requested_mode.clone(),
                description: description.clone(),
            })
        };

        // Notify TUI to create a child session in the sidebar.
        let _ = self
            .tool_event_tx
            .send(ToolEvent::SubagentStarted {
                call_id: call.id.clone(),
                handle_id: handle_id.clone(),
                description: description.clone(),
                prompt: prompt.clone(),
            })
            .await;

        debug!(
            handle = %handle_id,
            prompt = %prompt,
            mode = %requested_mode,
            acp_mode = %acp_mode,
            "task: spawning ACP sub-agent"
        );

        // Cancel channel: the receiver travels into the OS thread so it can
        // forward ACP `session/cancel` to the child when the parent cancels.
        // The CancelGuard fires the sender when `execute` returns or is dropped.
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
        let _cancel_guard = CancelGuard(Some(cancel_tx));

        let args = SpawnArgs {
            exe,
            prompt,
            description: description.clone(),
            mode: acp_mode,
            workdir,
            model_override,
            handle_id: handle_id.clone(),
            call_id: call.id.clone(),
            buffer_store: Arc::clone(&self.buffer_store),
            tool_event_tx: self.tool_event_tx.clone(),
            cancel_rx,
        };
        // Subagents always run with depth 0.
        let depth_for_env = 0u32;

        // The ACP ClientSideConnection is !Send (uses LocalBoxFuture internally).
        // We run the entire ACP session in a dedicated OS thread with its own
        // single-threaded tokio runtime + LocalSet.  Using a plain std::thread
        // avoids any interaction between the outer multi-threaded runtime and the
        // inner single-threaded one.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel::<ToolOutput>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build sub-agent runtime");
            let local = tokio::task::LocalSet::new();
            let output = rt.block_on(local.run_until(run_acp_session(args, depth_for_env)));
            let _ = result_tx.send(output);
        });

        result_rx
            .await
            .unwrap_or_else(|_| ToolOutput::err(&handle_id, "sub-agent thread died unexpectedly"))
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};

    use sven_tools::tool::{Tool, ToolCall};
    use sven_tools_fs::OutputBufferStore;
    use sven_workspace::{AgentInfo, SharedAgents};

    use super::{resolve_mode_and_prompt, TaskTool};

    fn make_task() -> TaskTool {
        make_task_with_agents(SharedAgents::empty())
    }

    fn make_task_with_agents(agents: SharedAgents) -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        TaskTool::new(store, tx, None, agents)
    }

    fn persona(name: &str, content: &str, readonly: bool) -> AgentInfo {
        AgentInfo {
            name: name.to_string(),
            description: format!("{name} persona"),
            model: None,
            readonly,
            is_background: false,
            content: content.to_string(),
            agent_md_path: std::path::PathBuf::from(format!("agents/{name}.md")),
            knowledge: vec![],
        }
    }

    fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "task".into(),
            args,
        }
    }

    #[test]
    fn name_is_task() {
        assert_eq!(make_task().name(), "task");
    }

    #[tokio::test]
    async fn status_action_missing_handle_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "status"}))).await;
        assert!(out.is_error, "expected error, got: {}", out.content);
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn read_action_missing_handle_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "read"}))).await;
        assert!(out.is_error);
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn grep_action_missing_handle_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "pattern": "foo"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("handle"));
    }

    #[tokio::test]
    async fn grep_action_missing_pattern_is_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "grep", "handle": "buf_0001"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("pattern"));
    }

    #[tokio::test]
    async fn status_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "status", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn read_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"action": "read", "handle": "buf_9999"})))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn grep_action_with_unknown_handle_returns_error() {
        let t = make_task();
        let out = t
            .execute(&call(json!({
                "action": "grep",
                "handle": "buf_9999",
                "pattern": "foo"
            })))
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_missing_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"action": "spawn"}))).await;
        assert!(
            out.is_error,
            "missing prompt should be an error: {}",
            out.content
        );
        assert!(out.content.contains("prompt"));
    }

    #[tokio::test]
    async fn spawn_null_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": null}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_integer_prompt_is_error() {
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": 42}))).await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn spawn_blocked_when_subagent() {
        let _env = std::env::var(super::DEPTH_ENV).ok();
        std::env::set_var(super::DEPTH_ENV, "0");
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
        std::env::remove_var(super::DEPTH_ENV);
        assert!(
            out.is_error,
            "spawn should be blocked when running as subagent: {}",
            out.content
        );
        assert!(
            out.content.contains("sub-agent") || out.content.contains("spawn"),
            "error should mention sub-agent: {}",
            out.content
        );
    }

    #[test]
    fn resolve_builtin_modes_pass_through_unchanged() {
        for m in ["research", "plan", "agent"] {
            let (mode, prompt) = resolve_mode_and_prompt(m, "do it", &[]).unwrap();
            assert_eq!(mode, m);
            assert_eq!(prompt, "do it");
        }
    }

    #[test]
    fn resolve_unknown_mode_without_personas_is_error() {
        let err = resolve_mode_and_prompt("bogus", "do it", &[]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("research"));
    }

    #[test]
    fn resolve_persona_readonly_maps_to_research_and_prepends_content() {
        let agents = [persona("knowledge-extract", "You extract knowledge.", true)];
        let (mode, prompt) =
            resolve_mode_and_prompt("knowledge-extract", "learn from doc.md", &agents).unwrap();
        assert_eq!(mode, "research");
        assert!(prompt.starts_with("You extract knowledge."));
        assert!(prompt.contains("learn from doc.md"));
    }

    #[test]
    fn resolve_persona_writable_maps_to_agent() {
        let agents = [persona("implementer", "You write code.", false)];
        let (mode, _prompt) =
            resolve_mode_and_prompt("implementer", "fix the bug", &agents).unwrap();
        assert_eq!(mode, "agent");
    }

    #[test]
    fn resolve_unknown_mode_lists_persona_names_as_suggestions() {
        let agents = [persona("knowledge-extract", "You extract knowledge.", true)];
        let err = resolve_mode_and_prompt("typo-mode", "x", &agents).unwrap_err();
        assert!(err.contains("knowledge-extract"));
    }

    #[test]
    fn parameters_schema_mode_enum_includes_discovered_personas() {
        let agents = SharedAgents::new(vec![persona("knowledge-extract", "body", true)]);
        let t = make_task_with_agents(agents);
        let schema = t.parameters_schema();
        let mode_enum = schema["properties"]["mode"]["enum"]
            .as_array()
            .expect("mode enum present");
        let names: Vec<&str> = mode_enum.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"agent"));
        assert!(names.contains(&"knowledge-extract"));
    }

    #[tokio::test]
    async fn spawn_with_unknown_mode_returns_error_before_spawning() {
        let t = make_task();
        let out = t
            .execute(&call(json!({"prompt": "do something", "mode": "not-a-real-mode"})))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("not-a-real-mode"));
    }
}
