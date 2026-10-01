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
//! ## The child's contract
//!
//! A sub-agent never holds more than the session that started it. Its
//! [`ChildRunContract`] is what the parent's mode allows in every state (see
//! `mode_policy::session_ceiling`), with a deadline of
//! `agent.child_run_timeout_secs`:
//!
//! - The child runs only in an ACP mode whose own policy stays within that
//!   ceiling (`child_mode_allowed`); if the child refuses to switch to it,
//!   the run is abandoned and the child killed rather than left in its
//!   default, wider mode.
//! - A permission request the child sends names a tool; its capability is
//!   what the kernel holds that tool to (`capability_for_tool_name`: an
//!   unknown tool, such as an MCP one, is `NetworkAccess`). A call outside
//!   the contract is refused. Within it, a parent whose host asks about every
//!   call ([`ChildApprover::Host`], an IDE over ACP) puts it to the host; a
//!   parent under manual approval puts a call that is not read-only to its
//!   own approval gate ([`ChildApprover::Gate`]); everything else is allowed
//!   without anyone being asked.
//! - The child's server is started with the parent's disabled tools, turn
//!   budgets and the time left before the deadline, rounded up so the parent
//!   sees its deadline first (`sven acp serve --disable-tool
//!   --max-tool-rounds --max-output-tokens --wall-clock-secs
//!   --permission-timeout-secs`); without a deadline the child's own
//!   permission timeout applies, so no request waits unbounded.
//! - The tokens the child's turn used are passed on as
//!   `SubagentUpdate::TokensUsed`, to be charged with the parent's.
//!
//! ## Timeouts
//!
//! A pinned `tokio::time::Sleep` future is reset on every ACP notification.
//! If no notification arrives within `INACTIVITY_TIMEOUT` while no
//! permission request of the child is pending, or the contract's deadline
//! passes however busy the child is, ACP `session/cancel` is forwarded to
//! the child, the child is killed, and the tool returns an error. So does a
//! child that exits before its turn finished.
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
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tracing::debug;

use sven_hsm::ChildRunContract;
use sven_tool_api::{
    events::ToolEvent,
    policy::ApprovalPolicy,
    tool::{Tool, ToolCall, ToolOutput},
    PathScope, PermissionRequester,
};
use sven_tools_fs::{BufGrepTool, BufReadTool, BufStatusTool, BufferSource, OutputBufferStore};
use sven_vocab::{AgentMode, ApprovalMode};
use sven_workspace::{AgentInfo, SharedAgents};

mod report;
mod session;
mod updates;
use session::{run_acp_session, CancelGuard, SpawnArgs};

/// Environment variable set when running as a subagent (depth 0).
///
/// Public because it is the ONE definition of "this process is a sub-agent":
/// `ToolSetProfile::for_session` reads it to pick the sub-agent tool set, and
/// a second literal there would let the spawner and the spawned disagree.
pub(crate) const SUBAGENT_DEPTH_ENV: &str = "SVEN_SUBAGENT_DEPTH";

/// `_meta` key under which `sven acp serve` names, on every permission
/// request, the capability the call exercises (`"WriteFile"`, ...), so the
/// parent answering it knows what the call does.
pub const CAPABILITY_META_KEY: &str = "sven.capability";

// ── Who answers a sub-agent's requests ───────────────────────────────────────

/// Who answers a sub-agent's permission request that is not allowed outright.
#[derive(Clone)]
pub enum ChildApprover {
    /// The host's requester (an IDE over ACP). The host is asked about every
    /// call a tool's policy marks `Ask`, in the parent as in the child, so no
    /// child request is allowed without it.
    Host(Arc<dyn PermissionRequester>),
    /// The session's own approval gate (`GateApprover`), asked under manual
    /// approval about what the session's policy does not allow without
    /// asking.
    Gate(Arc<dyn PermissionRequester>),
}

impl ChildApprover {
    /// Who answers a sub-agent's requests in a session with `host`'s
    /// requester (an IDE over ACP), under `approval`: the host when there is
    /// one, the session's own approval gate (`approvals`) under manual
    /// approval, and nobody under auto - there the contract settles every
    /// request.
    pub(crate) fn for_session(
        host: Option<Arc<dyn PermissionRequester>>,
        approval: ApprovalMode,
        approvals: &mpsc::Sender<sven_executors::ApprovalRequest>,
        tools: &crate::config::ToolsConfig,
    ) -> Option<Self> {
        match (host, approval) {
            (Some(host), _) => Some(Self::Host(host)),
            (None, ApprovalMode::Manual) => Some(Self::Gate(Arc::new(
                crate::session_handles::GateApprover::new(approvals.clone())
                    .with_preapproval(crate::mode_policy::preapproval(tools)),
            ))),
            (None, ApprovalMode::Auto) => None,
        }
    }

    /// The requester that is asked.
    pub(crate) fn requester(&self) -> &dyn PermissionRequester {
        match self {
            Self::Host(r) | Self::Gate(r) => r.as_ref(),
        }
    }

    /// `true` when the parent itself asks about every `Ask` call.
    pub(crate) fn asks_every_call(&self) -> bool {
        matches!(self, Self::Host(_))
    }
}

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
    /// The parent session's live mode (the same lock `SystemTool` switches).
    /// Read on every spawn: a child never gets more authority than its parent
    /// has at that moment - see [`child_mode_allowed`].
    parent_mode: Arc<Mutex<AgentMode>>,
    /// The session's path scope: a confined one is the tree a child's
    /// `workdir` must stay inside, and where the child starts by default.
    scope: PathScope,
    /// Longest a child may run, if bounded.
    wall_clock: Option<Duration>,
    /// Most tool rounds a child's turn may take, if limited.
    max_tool_rounds: Option<u32>,
    /// Most output tokens a child's response may produce, if limited.
    max_output_tokens: Option<u32>,
    /// Answers a child's permission request the contract does not settle on
    /// its own, when the host can ask someone.
    approver: Option<ChildApprover>,
    /// The session's approval mode; a child's contract carries it.
    approval_mode: ApprovalMode,
    /// Tools the session never runs, so its children never run them either.
    disabled_tools: Vec<String>,
    /// The session's `tools.deny_patterns` and `tools.auto_approve_patterns`.
    command_patterns: Option<(Vec<String>, Vec<String>)>,
}

impl TaskTool {
    pub fn new(
        buffer_store: Arc<Mutex<OutputBufferStore>>,
        tool_event_tx: mpsc::Sender<ToolEvent>,
        default_model: Option<String>,
        agents: SharedAgents,
        parent_mode: Arc<Mutex<AgentMode>>,
    ) -> Self {
        Self {
            buffer_store,
            tool_event_tx,
            default_model,
            agents,
            parent_mode,
            scope: PathScope::default(),
            wall_clock: None,
            max_tool_rounds: None,
            max_output_tokens: None,
            approver: None,
            approval_mode: ApprovalMode::Auto,
            disabled_tools: Vec::new(),
            command_patterns: None,
        }
    }

    /// Holds each child to these turn budgets - the session's own.
    #[must_use]
    pub fn with_turn_budgets(
        mut self,
        max_tool_rounds: Option<u32>,
        max_output_tokens: Option<u32>,
    ) -> Self {
        self.max_tool_rounds = max_tool_rounds;
        self.max_output_tokens = max_output_tokens;
        self
    }

    /// Stops each child after `budget` of wall-clock time, if bounded.
    #[must_use]
    pub fn with_wall_clock(mut self, budget: Option<Duration>) -> Self {
        self.wall_clock = budget;
        self
    }

    /// Holds each child to the session's `approval_mode`, and puts the child
    /// permission requests the contract does not allow outright to
    /// `approver` instead of refusing them.
    #[must_use]
    pub fn with_approver(
        mut self,
        approver: Option<ChildApprover>,
        approval_mode: ApprovalMode,
    ) -> Self {
        self.approver = approver;
        self.approval_mode = approval_mode;
        self
    }

    /// Keeps each child from running `tools`, as the session does not run
    /// them (`tools.disabled`).
    #[must_use]
    pub fn with_disabled_tools(mut self, tools: Vec<String>) -> Self {
        self.disabled_tools = tools;
        self
    }

    /// Holds each child to the session's shell-command patterns.
    #[must_use]
    pub fn with_command_patterns(mut self, tools: &crate::config::ToolsConfig) -> Self {
        let patterns = (
            tools.deny_patterns.clone(),
            tools.auto_approve_patterns.clone(),
        );
        self.command_patterns = Some(patterns);
        self
    }

    /// Holds children inside `scope`'s root rather than the process working
    /// directory.
    #[must_use]
    pub fn with_scope(mut self, scope: PathScope) -> Self {
        self.scope = scope;
        self
    }
}

/// Whether a parent in `parent` mode may start a child in ACP mode `child`.
///
/// The child mode's own policy - including what its tools do without asking
/// anyone - must stay within what the parent holds in every state (the
/// contract's ceiling, `mode_policy::session_ceiling`). An `agent` child
/// writes, so only an `agent` parent may start one; an SDLC parent holds only
/// reads in every state, so it may start no child at all. This check is what
/// makes the tool's `ReadFile` kernel capability honest: the kernel admits a
/// `task` call wherever reads are allowed, so the tool itself must not turn a
/// read into more.
pub(crate) fn child_mode_allowed(parent: AgentMode, child: &str) -> Result<(), String> {
    use crate::mode_policy::{exceeds, session_ceiling};
    let child_mode = match child {
        "research" => AgentMode::Research,
        "plan" => AgentMode::Plan,
        _ => AgentMode::Agent,
    };
    match exceeds(&session_ceiling(child_mode), &session_ceiling(parent)) {
        None => Ok(()),
        Some(capability) => Err(format!(
            "a {parent:?} session cannot delegate to a '{child}' sub-agent: the sub-agent \
             would hold {capability:?}, which this session does not"
        )),
    }
}

/// The contract a child of a session in `parent` mode under `approval` runs
/// under, started at `now`.
fn child_contract(
    parent: AgentMode,
    approval: ApprovalMode,
    wall_clock: Option<Duration>,
    now: Instant,
) -> ChildRunContract {
    let ceiling = crate::mode_policy::session_ceiling(parent);
    let contract = ChildRunContract::new(crate::mode_policy::with_approval(ceiling, approval));
    match wall_clock {
        Some(budget) => contract.with_deadline_after(now, budget),
        None => contract,
    }
}

/// The directory the child runs in: `requested` resolved through `session`,
/// the scope it must stay inside, or the scope's root when none is named. A
/// child confined to the parent's tree cannot be pointed at another
/// repository by the model.
fn child_workdir(requested: Option<&str>, session: &PathScope) -> Result<PathBuf, String> {
    let requested = requested.unwrap_or(".");
    let dir = session.resolve(requested).map_err(|e| e.to_string())?;
    if dir.is_dir() {
        Ok(dir)
    } else {
        Err(format!(
            "workdir '{requested}' is not an existing directory"
        ))
    }
}

/// What a requested `mode` resolves to: the ACP session mode to run, the
/// prompt actually sent to the sub-agent, and the model the persona asks for.
#[derive(Debug)]
struct Resolved {
    /// Built-in ACP session mode (`research` / `plan` / `agent`).
    acp_mode: String,
    /// Prompt with the persona's system prompt prepended, if any.
    prompt: String,
    /// Persona's `model:` front-matter, already normalised (`inherit` -> `None`).
    model: Option<String>,
}

/// Resolve a requested `mode` against the three built-in modes and any
/// discovered subagent persona.
///
/// A persona match prepends its `content` (system prompt) to `prompt`, carries
/// its `model` through for [`effective_model`], and maps `readonly` to an ACP
/// mode: `true` -> `research` (read-only tool profile), `false` -> `agent`
/// (full read/write tool profile). Built-in mode names always take priority
/// over a same-named persona.
fn resolve_mode_and_prompt(
    mode: &str,
    prompt: &str,
    agents: &[AgentInfo],
) -> Result<Resolved, String> {
    if matches!(mode, "research" | "plan" | "agent") {
        return Ok(Resolved {
            acp_mode: mode.to_string(),
            prompt: prompt.to_string(),
            model: None,
        });
    }

    if let Some(persona) = agents.iter().find(|a| a.name == mode) {
        let acp_mode = if persona.readonly {
            "research"
        } else {
            "agent"
        };
        return Ok(Resolved {
            acp_mode: acp_mode.to_string(),
            prompt: format!("{}\n\n---\n\n## Task\n\n{prompt}", persona.content.trim()),
            model: persona.model.clone(),
        });
    }

    let mut valid: Vec<&str> = vec!["research", "plan", "agent"];
    valid.extend(agents.iter().map(|a| a.name.as_str()));
    Err(format!(
        "unknown mode '{mode}'. Valid options: {}",
        valid.join(", ")
    ))
}

/// Pick the model a sub-agent runs on, most specific wins: the caller's
/// explicit `model` argument, then the persona's `model:` front-matter, then
/// the session's own model.  `None` leaves the child to its own default.
fn effective_model(
    explicit: Option<String>,
    persona: Option<String>,
    session_default: Option<String>,
) -> Option<String> {
    explicit.or(persona).or(session_default)
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        "Run a focused sub-agent to completion and return its final answer, or inspect\n\
         a finished one's output. The 'action' parameter lists what it can do.\n\
         'prompt' is required for spawning (action=spawn, the default); 'handle' is\n\
         required for inspecting an existing buffer (action=status|read|grep).\n\
         Spawning blocks until the sub-agent finishes; several may run at once by\n\
         issuing several calls.\n\
         'mode' takes a built-in role (research/plan/agent) or the name of a discovered\n\
         subagent persona, whose own instructions then become the sub-agent's system\n\
         prompt and whose readonly flag picks its tools.\n\
         Worth it when the work is open-ended search over unfamiliar ground and only the\n\
         conclusion matters - it keeps what was read out of this conversation. Not worth\n\
         it for a single command or a file you could read yourself: the sub-agent pays\n\
         the full system prompt again to do it.\n\
         Sub-agents get the standard tools and cannot spawn further sub-agents."
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
            // What `execute` refuses a call without: spawn needs `prompt`,
            // buffer actions need `handle`. Stating it here keeps a
            // schema-conformant model out of a guaranteed error round.
            "required": ["prompt", "handle"],
            "additionalProperties": false
        })
    }

    fn default_policy(&self) -> ApprovalPolicy {
        ApprovalPolicy::Ask
    }

    /// Admitted wherever reads are: what the sub-agent itself may do is held
    /// to the parent's ceiling by its contract (see `child_mode_allowed`).
    fn kernel_capability(&self) -> sven_tool_api::ToolCapability {
        sven_tool_api::ToolCapability::ReadFile
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

        // The session's own root when it has one; otherwise the working
        // directory, which a child is held inside all the same.
        let session = match self.scope.root() {
            Some(_) => self.scope.clone(),
            None => match std::env::current_dir().and_then(PathScope::confined) {
                Ok(scope) => scope,
                Err(e) => {
                    return ToolOutput::err(
                        &call.id,
                        format!("cannot resolve the working directory: {e}"),
                    )
                }
            },
        };
        let workdir =
            match child_workdir(call.args.get("workdir").and_then(|v| v.as_str()), &session) {
                Ok(dir) => dir,
                Err(e) => return ToolOutput::err(&call.id, e),
            };

        let requested_model = call
            .args
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // `acp_mode` is the built-in ACP session mode a persona maps onto;
        // `requested_mode` (kept for the TUI buffer label) may be a persona
        // name such as "code-reviewer".
        let resolved = match resolve_mode_and_prompt(&requested_mode, &prompt, &self.agents.get()) {
            Ok(v) => v,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let acp_mode = resolved.acp_mode;
        let parent_mode = *self.parent_mode.lock().await;
        if let Err(e) = child_mode_allowed(parent_mode, &acp_mode) {
            return ToolOutput::err(&call.id, e);
        }
        let prompt = resolved.prompt;
        let model_override =
            effective_model(requested_model, resolved.model, self.default_model.clone());

        // Subagents (SUBAGENT_DEPTH_ENV set) cannot spawn further sub-agents.
        if std::env::var(SUBAGENT_DEPTH_ENV).is_ok() {
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
            contract: ChildRunContract {
                max_tool_rounds: self.max_tool_rounds,
                max_output_tokens: self.max_output_tokens,
                ..child_contract(
                    parent_mode,
                    self.approval_mode,
                    self.wall_clock,
                    Instant::now(),
                )
            },
            approver: self.approver.clone(),
            terms: session::ServeTerms {
                disabled_tools: self.disabled_tools.clone(),
                approval: self.approval_mode,
                command_patterns: self.command_patterns.clone(),
            },
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

    use sven_tool_api::tool::{Tool, ToolCall};
    use sven_tools_fs::OutputBufferStore;
    use sven_workspace::{AgentInfo, SharedAgents};

    use super::{resolve_mode_and_prompt, TaskTool};
    use sven_vocab::{AgentMode, ApprovalMode};

    fn make_task() -> TaskTool {
        make_task_with_agents(SharedAgents::empty())
    }

    fn make_task_with_agents(agents: SharedAgents) -> TaskTool {
        make_task_in(AgentMode::Agent, agents)
    }

    /// A task tool whose parent session is in `mode`.
    fn make_task_in(mode: AgentMode, agents: SharedAgents) -> TaskTool {
        let (tx, _rx) = mpsc::channel(8);
        let store = Arc::new(Mutex::new(OutputBufferStore::new()));
        TaskTool::new(store, tx, None, agents, Arc::new(Mutex::new(mode)))
    }

    /// A read-only parent must never delegate to a child with more authority
    /// than it has itself: the spawn is refused before any process starts.
    #[tokio::test]
    async fn a_read_only_parent_cannot_spawn_a_writing_child() {
        for parent in [AgentMode::Research, AgentMode::Plan, AgentMode::Chat] {
            let task = make_task_in(parent, SharedAgents::empty());
            let out = task
                .execute(&call(json!({"prompt": "edit main.rs", "mode": "agent"})))
                .await;
            assert!(out.is_error, "{parent:?} parent spawned a writing child");
            assert!(
                out.content.contains("cannot delegate"),
                "refusal must say why: {}",
                out.content
            );
        }
    }

    /// A persona that is not read-only resolves to the writing mode, so it is
    /// refused under a read-only parent exactly like `mode: agent`.
    #[tokio::test]
    async fn a_read_only_parent_cannot_spawn_a_writing_persona() {
        let agents = SharedAgents::new(vec![persona("fixer", "fix things", false)]);
        let task = make_task_in(AgentMode::Research, agents);
        let out = task
            .execute(&call(json!({"prompt": "fix it", "mode": "fixer"})))
            .await;
        assert!(out.is_error && out.content.contains("cannot delegate"));
    }

    #[test]
    fn a_child_contract_is_the_parent_authority_with_a_deadline() {
        use std::time::{Duration, Instant};
        use sven_hsm::ToolCapability;
        let now = Instant::now();
        let budget = Some(Duration::from_secs(30));
        let child = super::child_contract(AgentMode::Research, ApprovalMode::Auto, budget, now);
        assert!(child.allows_without_asking(ToolCapability::ReadFile));
        assert!(!child
            .policy
            .allows_in_every_state(ToolCapability::WriteFile));
        assert_eq!(child.remaining(now), budget);
        let child = super::child_contract(AgentMode::Agent, ApprovalMode::Auto, None, now);
        assert!(child.allows_without_asking(ToolCapability::WriteFile));
        assert!(child.allows_without_asking(ToolCapability::ExecuteShell));
        assert_eq!(child.deadline, None);
        let child = super::child_contract(AgentMode::Agent, ApprovalMode::Manual, None, now);
        assert!(child.allows_without_asking(ToolCapability::ReadFile));
        assert!(!child.allows_without_asking(ToolCapability::WriteFile));
        assert!(child
            .policy
            .allows_in_every_state(ToolCapability::WriteFile));
    }

    #[test]
    fn the_child_mode_ceiling_follows_the_parent_mode() {
        use super::child_mode_allowed;
        for child in ["research", "plan", "agent"] {
            assert!(
                child_mode_allowed(AgentMode::Agent, child).is_ok(),
                "agent -> {child}"
            );
            // An SDLC session holds only ReadFile in every state; every child
            // mode can do more (a research child fetches the web), so none
            // may be started from it.
            assert!(
                child_mode_allowed(AgentMode::Sdlc, child).is_err(),
                "sdlc -> {child}"
            );
        }
        for parent in [AgentMode::Research, AgentMode::Plan, AgentMode::Chat] {
            assert!(child_mode_allowed(parent, "research").is_ok());
            assert!(child_mode_allowed(parent, "plan").is_ok());
            assert!(
                child_mode_allowed(parent, "agent").is_err(),
                "{parent:?} -> agent"
            );
        }
    }

    /// The child runs inside the parent's working tree, never wherever the
    /// model points it.
    #[tokio::test]
    async fn a_child_workdir_outside_the_session_root_is_refused() {
        let task = make_task();
        let out = task
            .execute(&call(json!({"prompt": "look around", "workdir": "../.."})))
            .await;
        assert!(
            out.is_error,
            "a workdir outside the session root was accepted"
        );
        assert!(out.content.contains("outside"), "{}", out.content);
    }

    /// A persona that declares a `model:`, for the model-precedence specs.
    pub(super) fn persona_with_model(name: &str, model: Option<&str>) -> AgentInfo {
        AgentInfo {
            model: model.map(str::to_string),
            ..persona(name, "persona body", false)
        }
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
        let _env = std::env::var(super::SUBAGENT_DEPTH_ENV).ok();
        std::env::set_var(super::SUBAGENT_DEPTH_ENV, "0");
        let t = make_task();
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
        std::env::remove_var(super::SUBAGENT_DEPTH_ENV);
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
            let r = resolve_mode_and_prompt(m, "do it", &[]).unwrap();
            assert_eq!(r.acp_mode, m);
            assert_eq!(r.prompt, "do it");
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
        let agents = [persona("code-reviewer", "You review code.", true)];
        let r = resolve_mode_and_prompt("code-reviewer", "review src/lib.rs", &agents).unwrap();
        assert_eq!(r.acp_mode, "research");
        assert!(r.prompt.starts_with("You review code."));
        assert!(r.prompt.contains("review src/lib.rs"));
    }

    #[test]
    fn resolve_persona_writable_maps_to_agent() {
        let agents = [persona("implementer", "You write code.", false)];
        let r = resolve_mode_and_prompt("implementer", "fix the bug", &agents).unwrap();
        assert_eq!(r.acp_mode, "agent");
    }

    #[test]
    fn resolve_unknown_mode_lists_persona_names_as_suggestions() {
        let agents = [persona("code-reviewer", "You review code.", true)];
        let err = resolve_mode_and_prompt("typo-mode", "x", &agents).unwrap_err();
        assert!(err.contains("code-reviewer"));
    }

    #[test]
    fn parameters_schema_mode_enum_includes_discovered_personas() {
        let agents = SharedAgents::new(vec![persona("code-reviewer", "body", true)]);
        let t = make_task_with_agents(agents);
        let schema = t.parameters_schema();
        let mode_enum = schema["properties"]["mode"]["enum"]
            .as_array()
            .expect("mode enum present");
        let names: Vec<&str> = mode_enum.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"agent"));
        assert!(names.contains(&"code-reviewer"));
    }

    /// The schema must state every parameter `execute` refuses a call for:
    /// `prompt` is mandatory for spawn, `handle` for status/read/grep. A
    /// model conforming to a schema silent about this produces a guaranteed
    /// error round - observed as a real 8-call retry storm.
    #[test]
    fn the_schema_requires_what_execute_validates() {
        let t = make_task();
        let schema = t.parameters_schema();
        let required = schema["required"]
            .as_array()
            .expect("required list present");
        let names: Vec<&str> = required.iter().filter_map(|v| v.as_str()).collect();
        assert!(names.contains(&"prompt"), "prompt must be required");
        assert!(names.contains(&"handle"), "handle must be required");
    }

    #[tokio::test]
    async fn spawn_with_unknown_mode_returns_error_before_spawning() {
        let t = make_task();
        let out = t
            .execute(&call(
                json!({"prompt": "do something", "mode": "not-a-real-mode"}),
            ))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("not-a-real-mode"));
    }
}

#[cfg(test)]
mod persona_model_tests {
    use super::tests::persona_with_model;
    use super::{effective_model, resolve_mode_and_prompt};

    #[test]
    fn resolve_returns_the_personas_model() {
        let agents = [persona_with_model("fast-reviewer", Some("fast"))];
        let r = resolve_mode_and_prompt("fast-reviewer", "review it", &agents).unwrap();
        assert_eq!(r.model.as_deref(), Some("fast"));
    }

    #[test]
    fn builtin_modes_carry_no_model() {
        let r = resolve_mode_and_prompt("agent", "do it", &[]).unwrap();
        assert!(r.model.is_none());
    }

    #[test]
    fn explicit_override_beats_persona_which_beats_session_default() {
        assert_eq!(
            effective_model(
                Some("opus".into()),
                Some("fast".into()),
                Some("sonnet".into())
            ),
            Some("opus".into())
        );
        assert_eq!(
            effective_model(None, Some("fast".into()), Some("sonnet".into())),
            Some("fast".into())
        );
        assert_eq!(
            effective_model(None, None, Some("sonnet".into())),
            Some("sonnet".into())
        );
        assert_eq!(effective_model(None, None, None), None);
    }
}
