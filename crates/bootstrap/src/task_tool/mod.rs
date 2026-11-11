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
//! A `CancelGuard` RAII type holds the cancel sender.  When `execute` is
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
    PathScope,
};
use sven_tools_fs::{BufferSource, OutputBufferStore};
use sven_vocab::{AgentMode, ApprovalMode};
use sven_workspace::SharedAgents;
mod admission;
mod approver;
mod brief;
mod finding;
mod handback;
mod inspect;
mod isolation;
pub(crate) mod orchestrate;
mod permission_client;
mod report;
mod resolve;
mod runner;
mod session;
mod subgoals;
mod terms;
mod updates;
mod voi;
mod weigh;
mod worktree;
pub use approver::ChildApprover;
use isolation::Isolation;
pub(crate) use resolve::child_mode_allowed;
use resolve::{child_contract, child_workdir, effective_model, resolve_mode_and_prompt};
use runner::{AcpProcessRunner, ChildRunner};
use session::{CancelGuard, SpawnArgs};
pub(crate) use subgoals::NearMatch;
pub use terms::{ParentTerms, TermsEndpoint};

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

/// `_meta` key under which `sven acp serve` names, on a finished tool call,
/// the files the call made or names, by handle (`ArtifactRef`s), so the
/// parent relays them with its own result and its client shows them.
pub const ARTIFACTS_META_KEY: &str = "sven.artifacts";

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
    /// The session's chunk store, when addressable context is on: where a
    /// brief's evidence is read from.
    chunks: Option<Arc<sven_executors::ChunkSession>>,
    /// What each finished sub-agent cost and what its result spared the parent.
    ledger: Arc<handback::ChildLedger>,
    /// The subgoals handed out so far, to recognise a repeat.
    subgoals: Arc<subgoals::SubgoalRegistry>,
    /// Asked about a near-identical read-only goal; without one it launches.
    near_match: Option<Arc<dyn NearMatch>>,
    /// Whether a subtask a sub-agent would not pay for is declined.
    delegation: crate::config::DelegationConfig,
    /// Whether this process is itself a sub-agent (its parent set
    /// [`SUBAGENT_DEPTH_ENV`]); a sub-agent never spawns another. Read once
    /// when the tool is built, so no call depends on the process environment
    /// at the moment it runs.
    is_subagent: bool,
    /// Where the session runs models, and the conversation it names there:
    /// a child is handed files through it and makes its own there.
    media: Option<(
        sven_tools_media::MediaEndpoint,
        sven_tools_media::MediaConfig,
    )>,
    /// What a parent held to an allow list hands each child (`ParentTerms`).
    terms: Option<ParentTerms>,
    /// Runs a prepared child to its end; the production one starts a
    /// `sven acp serve` process.
    runner: Arc<dyn ChildRunner>,
    /// Where isolated children's worktrees live, outside every parent tree.
    worktree_root: PathBuf,
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
            media: None,
            terms: None,
            runner: Arc::new(AcpProcessRunner),
            worktree_root: isolation::default_worktree_root(),
            chunks: None,
            ledger: Arc::default(),
            subgoals: Arc::default(),
            near_match: None,
            delegation: crate::config::DelegationConfig::default(),
            is_subagent: std::env::var(SUBAGENT_DEPTH_ENV).is_ok(),
        }
    }

    /// Lets `advisor` say whether two near-identical read-only goals are one.
    #[must_use]
    pub(crate) fn with_near_match(mut self, advisor: Option<Arc<dyn NearMatch>>) -> Self {
        self.near_match = advisor;
        self
    }

    /// Holds each call to `delegation.policy` and its numbers.
    #[must_use]
    pub fn with_delegation(mut self, delegation: crate::config::DelegationConfig) -> Self {
        self.delegation = delegation;
        self
    }

    /// Lets a call brief its sub-agent from the chunks of `session`'s store.
    #[must_use]
    pub fn with_chunks(mut self, session: Option<Arc<sven_executors::ChunkSession>>) -> Self {
        self.chunks = session;
        self
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

    /// Hands children files through the session's model service, and has
    /// what they make belong to the same conversation.
    #[must_use]
    pub fn with_media(
        mut self,
        media: Option<(
            sven_tools_media::MediaEndpoint,
            sven_tools_media::MediaConfig,
        )>,
    ) -> Self {
        self.media = media;
        self
    }

    /// Holds each child to `terms` (its parent's allow list and endpoints).
    #[must_use]
    pub fn with_parent_terms(mut self, terms: Option<ParentTerms>) -> Self {
        self.terms = terms;
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

    /// Makes isolated children's worktrees under `root`.
    #[cfg(test)]
    fn with_worktree_root(mut self, root: PathBuf) -> Self {
        self.worktree_root = root;
        self
    }

    /// Runs children with `runner` instead of a `sven acp serve` process.
    #[cfg(test)]
    fn with_runner(mut self, runner: Arc<dyn ChildRunner>) -> Self {
        self.runner = runner;
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

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn summary(&self) -> &str {
        "Delegate a focused sub-task to a sub-agent and get its final answer."
    }

    fn description(&self) -> &str {
        "Run a focused sub-agent to completion and return its final answer (action=spawn,\n\
         the default; 'prompt' required), or inspect a finished one's output\n\
         (action=status|read|grep; 'handle' required), or list the discovered subagents\n\
         with their descriptions (action=agents).\n\
         Spawning blocks until the sub-agent finishes; issue several calls to run several\n\
         at once; when several writing sub-agents run at once, give each\n\
         isolation=worktree so they cannot overwrite each other. 'mode' is research, plan, agent or the name of a discovered subagent\n\
         persona, whose instructions become the sub-agent's system prompt and whose\n\
         readonly flag picks its tools.\n\
         Worth it for open-ended search over unfamiliar ground when only the conclusion\n\
         matters. Not worth it for a single command or a file you could read yourself:\n\
         the sub-agent pays for the full system prompt again. Sub-agents get the\n\
         standard tools and cannot spawn further sub-agents."
    }

    fn parameters_schema(&self) -> Value {
        let mut mode_enum: Vec<String> = vec!["research".into(), "plan".into(), "agent".into()];
        mode_enum.extend(self.agents.get().iter().map(|a| a.name.clone()));

        let mut schema = serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["spawn", "status", "read", "grep", "agents"],
                    "description": "spawn (default), inspect a buffer, or list the subagents"
                },
                "prompt": {
                    "type": "string",
                    "description": "[spawn] Complete, self-contained task (required for spawn)"
                },
                "description": {
                    "type": "string",
                    "description": "[spawn] Short label shown in the TUI"
                },
                "mode": {
                    "type": "string",
                    "enum": mode_enum,
                    "description": "[spawn] Default agent"
                },
                "workdir": {
                    "type": "string",
                    "description": "[spawn] Working directory (default: cwd)"
                },
                "model": {
                    "type": "string",
                    "description": "[spawn] Model override"
                },
                "isolation": {
                    "type": "string",
                    "enum": ["none", "worktree"],
                    "description": "[spawn] worktree: a writing child works in its own git worktree and its change is merged into your tree when it succeeds (conflicts are reported, never forced); default none"
                },
                "inputs": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "[spawn] Files it works on: handles (mcp-file://files/...) or paths; images are shown to it"
                },
                "handle": {
                    "type": "string",
                    "description": "[status|read|grep] Buffer handle from a spawn (required for these)"
                },
                "start_line": {
                    "type": "integer",
                    "description": "[read] First line, 1-indexed"
                },
                "end_line": {
                    "type": "integer",
                    "description": "[read] Last line, inclusive"
                },
                "pattern": {
                    "type": "string",
                    "description": "[grep] Regex"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "[grep] Context lines (default 2)"
                },
                "limit": {
                    "type": "integer",
                    "description": "[grep] Maximum matches (default 50)"
                }
            },
            // Nothing is required here: spawn needs `prompt` and the buffer
            // actions need `handle`, so a top-level list would force one onto
            // the other. `execute` validates per action; the property
            // descriptions say which action needs what.
            "additionalProperties": false
        });
        // The brief, the declared scope and the cost account belong to
        // sessions with addressable context; others keep the smaller schema.
        if self.chunks.is_some() {
            schema["properties"]["action"]["enum"] =
                serde_json::json!(["spawn", "status", "read", "grep", "agents", "ledger"]);
            schema["properties"]["evidence"] = serde_json::json!({
                "type": "array", "items": {"type": "string"},
                "description": "[spawn] Chunk ids (c-...) to start the sub-agent from"
            });
            schema["properties"]["evidence_visibility"] = serde_json::json!({
                "type": "string", "enum": ["hidden", "short", "long", "full"]
            });
            schema["properties"]["deliverable"] = serde_json::json!({
                "type": "string", "description": "[spawn] Answer shape wanted"
            });
            schema["properties"]["paths"] = serde_json::json!({
                "type": "array", "items": {"type": "string"},
                "description": "[spawn] Files or globs it works on"
            });
        }
        schema
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

        // ── Actions that do not spawn ─────────────────────────────────────────
        if let Some(output) = self.inspect(action, call).await {
            return output;
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
            .map_or_else(|| prompt.chars().take(60).collect(), str::to_string);

        let brief = match brief::briefed(&call.args, &prompt, self.chunks.as_deref()) {
            Ok(brief) => brief,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let original_prompt = prompt;
        let prompt = brief.prompt;

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

        let inputs: Vec<String> = match call.args.get("inputs") {
            None => Vec::new(),
            Some(serde_json::Value::Array(list)) => match list
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            {
                Some(list) => list,
                None => return ToolOutput::err(&call.id, "`inputs` holds handles or paths"),
            },
            Some(_) => return ToolOutput::err(&call.id, "`inputs` is a list of handles or paths"),
        };
        let handed = match sven_tools_media::hand_over(
            self.media.as_ref().map(|(e, c)| (e, c)),
            &workdir,
            &inputs,
        )
        .await
        {
            Ok(handed) => handed,
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
        let isolation = match Isolation::of(&call.args, &acp_mode) {
            Ok(isolation) => isolation,
            Err(e) => return ToolOutput::err(&call.id, e),
        };
        let prompt = resolved.prompt;
        let model_override =
            effective_model(requested_model, resolved.model, self.default_model.clone());

        if self.is_subagent {
            return ToolOutput::err(&call.id, "sub-agents cannot spawn further sub-agents");
        }

        let leader = match self
            .admit(&admission::Call {
                id: &call.id,
                args: &call.args,
                scope: &session,
                mode: &requested_mode,
                acp_mode: &acp_mode,
                prompt: &original_prompt,
                deliverable: brief.deliverable.as_deref(),
                evidence: &brief.evidence,
            })
            .await
        {
            Ok(leader) => leader,
            Err(answer) => return answer,
        };

        let note = match self.weigh(&weigh::Candidate {
            id: &call.id,
            args: &call.args,
            scope: &session,
            brief: &prompt,
            prompt: &original_prompt,
            writes: acp_mode == "agent",
        }) {
            weigh::Verdict::Launch { note } => note,
            weigh::Verdict::Decline(answer) => return answer,
        };

        // An isolated child runs in a private worktree of the parent's
        // current state; its change is merged back once it has finished.
        let mut workdir = workdir;
        let mut isolated = None;
        if isolation == Isolation::Worktree {
            match isolation::create_worktree(workdir.clone(), self.worktree_root.clone()).await {
                Ok(worktree) => {
                    workdir = worktree.workdir();
                    isolated = Some(worktree);
                }
                Err(e) => {
                    leader.finish(None);
                    return ToolOutput::err(&call.id, e);
                }
            }
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
            prompt: format!("{prompt}{}", handed.note),
            images: handed.images,
            media_session: self.media.as_ref().and_then(|(_, c)| c.session.clone()),
            parent_terms: self.terms.clone(),
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
                evidence_store: brief.evidence_store,
            },
            workdir,
            model_override,
            handle_id: handle_id.clone(),
            call_id: call.id.clone(),
            buffer_store: Arc::clone(&self.buffer_store),
            tool_event_tx: self.tool_event_tx.clone(),
            cancel_rx,
            reporting: handback::Reporting {
                structured: brief.structured,
                chunks: self.chunks.clone(),
                ledger: Arc::clone(&self.ledger),
            },
        };
        let mut output = self.runner.run(args).await;
        if let Some(worktree) = isolated {
            output = isolation::settle(worktree, output, &call.id).await;
        }
        leader.finish((!output.is_error).then(|| subgoals::Outcome {
            handle: handle_id.clone(),
            text: output.content.clone(),
        }));
        if let (Some(note), false) = (note, output.is_error) {
            output.content = format!("{}\n\n{note}", output.content);
        }
        output
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

    /// A parent held to an allow list (a hosted session) holds its children
    /// to the same list without `task`, and hands them its own endpoints;
    /// a parent held to none hands nothing, and its child reads its own.
    #[test]
    fn a_child_is_held_to_its_parents_allow_list_and_endpoints() {
        let mut parent = crate::Config::default();
        use super::ParentTerms;
        assert!(ParentTerms::of(&parent).is_none());
        parent.tools.only = Some(vec!["web_search".into(), "run_model".into(), "task".into()]);
        parent.tools.web.public_only = true;
        parent.tools.hide_locations = true;
        parent.model.base_url = Some("https://sven.example/v1".into());
        parent.model.api_key = Some("host-key".into());
        parent.model.provider = "openai".into();
        parent.tools.media.service = Some(sven_tools_media::MediaEndpoint {
            base_url: "https://sven.example/v1".into(),
            api_key: "host-key".into(),
        });
        parent.tools.media.session = Some("conv-1".into());
        let terms = ParentTerms::of(&parent).expect("a held parent hands terms");
        assert_eq!(terms.only_tools, ["web_search", "run_model"]);
        // Never the key in its debug form (a log line).
        assert!(!format!("{terms:?}").contains("host-key"));
        assert_eq!(terms.model_name("openai/e2e/agent"), "e2e/agent");
        assert_eq!(terms.model_name("e2e/agent"), "e2e/agent");

        let mut child: crate::Config = serde_yaml::from_str(
            "mcp_servers:\n  local: {transport: {type: stdio, command: /bin/true}}\n",
        )
        .unwrap();
        terms.apply(&mut child);
        // Its model calls name the parent's conversation, as its media calls do.
        assert_eq!(child.model.session.as_deref(), Some("conv-1"));
        assert_eq!(
            child.tools.only.as_deref(),
            Some(&["web_search".to_owned(), "run_model".to_owned()][..])
        );
        assert!(
            child.mcp_servers.is_empty()
                && child.tools.web.public_only
                && child.tools.hide_locations
        );
        assert_eq!(
            (
                child.model.provider.as_str(),
                child.model.api_key.as_deref()
            ),
            ("openai", Some("host-key"))
        );
        let (media, config) = child
            .media_endpoint()
            .expect("models run at the parent's service");
        assert_eq!(
            (media.api_key.as_str(), config.session.as_deref()),
            ("host-key", Some("conv-1"))
        );
        // Terms that somehow named `task` still give the child none.
        let mut sneaky = terms.clone();
        sneaky.only_tools.push("task".into());
        sneaky.apply(&mut child);
        assert!(!child.tools.only.unwrap().contains(&"task".to_owned()));
    }

    pub(super) fn make_task() -> TaskTool {
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
            activation: None,
        }
    }

    pub(super) fn call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "t1".into(),
            name: "task".into(),
            args,
        }
    }

    #[tokio::test]
    async fn the_agents_action_lists_every_persona_with_its_whole_description() {
        let agents = SharedAgents::new(vec![
            persona("code-reviewer", "body", true),
            persona_with_model("fixer", Some("fast")),
        ]);
        let out = make_task_with_agents(agents)
            .execute(&call(json!({"action": "agents"})))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.contains("code-reviewer persona") && out.content.contains("(readonly)"),
            "{}",
            out.content
        );
        assert!(
            out.content.contains("fixer") && out.content.contains("model: fast"),
            "{}",
            out.content
        );
    }

    #[tokio::test]
    async fn the_agents_action_says_so_when_there_are_no_personas() {
        let out = make_task()
            .execute(&call(json!({"action": "agents"})))
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("no subagents"), "{}", out.content);
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

    #[test]
    fn the_schema_offers_the_brief_fields_without_requiring_them() {
        let schema = make_task().parameters_schema();
        assert!(
            schema["properties"].get("evidence").is_none(),
            "no chunks, no brief fields"
        );
        let (store, _dir) = {
            let dir = tempfile::tempdir().unwrap();
            let s =
                sven_chunks::ChunkStore::open(&dir.path().join("s"), Default::default()).unwrap();
            let probe = sven_chunks::FsProbe::new(dir.path()).unwrap();
            (
                Arc::new(sven_executors::ChunkSession::new(
                    Arc::new(std::sync::Mutex::new(s)),
                    Arc::new(probe),
                )),
                dir,
            )
        };
        let schema = make_task().with_chunks(Some(store)).parameters_schema();
        for key in ["evidence", "evidence_visibility", "deliverable", "paths"] {
            assert!(schema["properties"].get(key).is_some(), "{key}");
            assert!(schema.get("required").is_none(), "{key} is not required");
        }
    }

    #[tokio::test]
    async fn a_brief_that_cannot_be_built_is_refused_before_anything_starts() {
        let t = make_task();
        for args in [
            json!({"prompt": "p", "evidence": ["../../etc/passwd"]}),
            json!({"prompt": "p", "evidence": ["c-0123456789abcdef"]}),
            json!({"prompt": "p", "evidence_visibility": "all", "deliverable": "x"}),
        ] {
            let out = t.execute(&call(args.clone())).await;
            assert!(out.is_error, "{args}: {}", out.content);
        }
    }

    /// A repeat of a read-only subgoal that is running waits for it and is
    /// answered by reference, without a second child.
    #[tokio::test]
    async fn a_repeated_read_only_subgoal_is_answered_by_reference() {
        use super::subgoals::{Access, Admission, Outcome, Subgoal, SubgoalSpec};
        let task = Arc::new(make_task());
        let spec = SubgoalSpec {
            mode: "research",
            prompt: "find the callers",
            deliverable: None,
            paths: &[],
            evidence: &[],
            access: Access::ReadOnly,
        };
        let scope = sven_tool_api::PathScope::confined(std::env::current_dir().unwrap()).unwrap();
        let Admission::Launch(leader) =
            task.subgoals.admit(Subgoal::new(&spec), &scope, None).await
        else {
            panic!("the first call launches")
        };
        let twin = {
            let task = Arc::clone(&task);
            tokio::spawn(async move {
                task.execute(&call(
                    json!({"prompt": "Find the callers.", "mode": "research"}),
                ))
                .await
            })
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while leader.waiters() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the repeated call waits on the running child");
        leader.finish(Some(Outcome {
            handle: "buf_0001".into(),
            text: "found".into(),
        }));
        let out = twin.await.unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Not launched again") && out.content.contains("buf_0001"));
        assert!(task.ledger.render().contains("launches avoided"));
    }

    #[tokio::test]
    async fn the_ledger_action_says_so_when_no_sub_agent_has_finished() {
        let out = make_task()
            .execute(&call(json!({"action": "ledger"})))
            .await;
        assert!(
            !out.is_error && out.content.contains("no sub-agent"),
            "{}",
            out.content
        );
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
        let mut t = make_task();
        t.is_subagent = true;
        let out = t.execute(&call(json!({"prompt": "do something"}))).await;
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

    /// `prompt` is needed by spawn only and `handle` by the buffer actions
    /// only, so neither may be required at the top level: a schema-conformant
    /// model would otherwise invent a `handle` for every spawn and a `prompt`
    /// for every status. `execute` refuses the call that lacks what its own
    /// action needs, and the property descriptions say which action does.
    #[tokio::test]
    async fn the_schema_requires_nothing_that_only_one_action_needs() {
        let t = make_task();
        let schema = t.parameters_schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(required.is_empty(), "top-level required: {required:?}");
        assert!(schema["properties"]["prompt"]["description"]
            .as_str()
            .unwrap()
            .contains("required"));
        assert!(schema["properties"]["handle"]["description"]
            .as_str()
            .unwrap()
            .contains("required"));
        // What the schema leaves optional, execute still enforces per action.
        let no_prompt = t.execute(&call(json!({"action": "spawn"}))).await;
        assert!(no_prompt.is_error && no_prompt.content.contains("'prompt'"));
        let no_handle = t.execute(&call(json!({"action": "status"}))).await;
        assert!(no_handle.is_error && no_handle.content.contains("handle"));
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
