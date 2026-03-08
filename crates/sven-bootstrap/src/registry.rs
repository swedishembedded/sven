// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Centralised tool-registry builder.
//!
//! All callers (CI runner, conversation runner, TUI, sub-agents) use
//! `build_tool_registry` with the appropriate [`ToolSetProfile`] instead of
//! each inlining their own registration loop.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::{AgentMode, Config};
use sven_model::ModelProvider;
use sven_runtime::Shared;
use sven_tools::{
    events::{TodoItem, ToolEvent},
    AskQuestionTool, BufGrepTool, BufReadTool, BufStatusTool, ContextGrepTool, ContextOpenTool,
    ContextReadTool, ContextStore, DeleteFileTool, EditFileTool, FindFileTool, GdbCommandTool,
    GdbConnectTool, GdbInterruptTool, GdbSessionState, GdbStartServerTool, GdbStatusTool,
    GdbStopTool, GdbWaitStoppedTool, GrepTool, ListDirTool, ListKnowledgeTool, LoadSkillTool,
    OutputBufferStore, ReadFileTool, ReadImageTool, ReadLintsTool, RunTerminalCommandTool,
    SearchCodebaseTool, SearchKnowledgeTool, ShellTool, SwitchModeTool, TodoWriteTool,
    ToolRegistry, UpdateMemoryTool, WebFetchTool, WebSearchTool, WriteTool,
};

use sven_core::AgentRuntimeContext;

use crate::context::ToolSetProfile;
use crate::context_query::build_context_query_tools;
use crate::task_tool::TaskTool;

/// Build a [`ToolRegistry`] populated according to the given `profile`.
///
/// This is the single canonical place where tools are wired up.  Adding a
/// new tool to sven means adding it here once and it will appear in every
/// appropriate profile automatically.
///
/// ### Shared-state parameters
///
/// * `mode_lock` — the **same** `Arc` that will be passed to `Agent::new()`.
///   `SwitchModeTool` holds a clone so that mode changes are immediately
///   visible to the agent loop via `drain_tool_events`.
/// * `tool_event_tx` — the sending half of the channel whose receiving end
///   is passed to `Agent::new()`.  `TodoWriteTool` and `SwitchModeTool` send
///   events here; the agent drains them after each tool execution.
/// * `sub_agent_runtime` — inherited by `TaskTool` sub-agents (project root,
///   CI/git notes, AGENTS.md).  Only used for the `Full` profile; pass
///   `AgentRuntimeContext::default()` otherwise.
/// * `buffer_store` — shared [`OutputBufferStore`] for `task`, `buf_read`,
///   `buf_grep`, `buf_status`.  Create once per session with
///   `Arc::new(Mutex::new(OutputBufferStore::new()))` and pass the same
///   instance to both this function and any code that needs to inspect buffers.
pub fn build_tool_registry(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    profile: ToolSetProfile,
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    sub_agent_runtime: AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> ToolRegistry {
    match profile {
        ToolSetProfile::Full { question_tx, todos } => {
            let mut reg = ToolRegistry::new();

            register_base_tools(
                &mut reg,
                cfg,
                model.clone(),
                mode_lock,
                tool_event_tx.clone(),
                &sub_agent_runtime,
                Arc::clone(&buffer_store),
            );

            // Only register ask_question when a TUI channel is available.
            // In headless/CI/sub-agent mode there is no UI to display the modal,
            // so we omit the tool entirely — the model won't attempt to call it.
            if let Some(tx) = question_tx {
                reg.register(AskQuestionTool::new_tui(tx));
            }
            reg.register(TodoWriteTool::new(todos, tool_event_tx.clone()));

            // TaskTool allows spawning local sub-agents.  Omitted from SubAgent
            // profile to prevent unbounded subprocess nesting.
            reg.register(TaskTool::new(
                Arc::clone(&buffer_store),
                tool_event_tx,
                Some(format!("{}/{}", cfg.model.provider, cfg.model.name)),
            ));

            reg
        }

        ToolSetProfile::SubAgent { todos } => {
            let mut reg = ToolRegistry::new();

            register_base_tools(
                &mut reg,
                cfg,
                model,
                mode_lock,
                tool_event_tx.clone(),
                &sub_agent_runtime,
                buffer_store,
            );

            // ask_question is intentionally omitted: sub-agents run headless
            // and have no UI channel to display the modal.
            // TaskTool is intentionally omitted to limit sub-agent nesting.
            reg.register(TodoWriteTool::new(todos, tool_event_tx));

            reg
        }
    }
}

/// Register the tool set shared by every agent profile (Full and SubAgent).
///
/// Covers file I/O, search, web, terminal, GDB, context (RLM), skills,
/// knowledge, and buffer read tools.  Profile-specific tools (`TaskTool`,
/// `AskQuestionTool`, `TodoWriteTool`) are **not** registered here — the
/// caller adds them after this call according to the profile.
fn register_base_tools(
    reg: &mut ToolRegistry,
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    mode_lock: Arc<Mutex<AgentMode>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) {
    // ── File ─────────────────────────────────────────────────────────────────
    reg.register(ReadFileTool);
    reg.register(ReadImageTool);
    reg.register(ListDirTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register(EditFileTool);
    reg.register(DeleteFileTool);

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool);
    reg.register(SearchCodebaseTool);

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(ReadLintsTool);
    reg.register(UpdateMemoryTool {
        memory_file: cfg.tools.memory.memory_file.clone(),
    });
    reg.register(SwitchModeTool::new(mode_lock, tool_event_tx.clone()));
    reg.register(RunTerminalCommandTool {
        timeout_secs: cfg.tools.timeout_secs,
    });
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
    });

    // ── Buffer access (shared with TaskTool) ─────────────────────────────────
    reg.register(BufReadTool::new(Arc::clone(&buffer_store)));
    reg.register(BufGrepTool::new(Arc::clone(&buffer_store)));
    reg.register(BufStatusTool::new(Arc::clone(&buffer_store)));

    // ── Skills and knowledge ──────────────────────────────────────────────────
    reg.register(LoadSkillTool::new(runtime.skills.clone()));
    reg.register(ListKnowledgeTool {
        knowledge: runtime.knowledge.clone(),
    });
    reg.register(SearchKnowledgeTool {
        knowledge: runtime.knowledge.clone(),
    });

    // ── Context (RLM memory-mapped large-file tools) ──────────────────────────
    let context_store = Arc::new(Mutex::new(ContextStore::new()));
    reg.register(ContextOpenTool::new(context_store.clone()));
    reg.register(ContextReadTool::new(context_store.clone()));
    reg.register(ContextGrepTool::new(context_store.clone()));
    let (ctx_query, ctx_reduce) =
        build_context_query_tools(context_store, model, cfg, Some(tool_event_tx));
    reg.register(ctx_query);
    reg.register(ctx_reduce);

    // ── GDB ───────────────────────────────────────────────────────────────────
    let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
    reg.register(GdbStartServerTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbConnectTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbCommandTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbInterruptTool::new(gdb_state.clone()));
    reg.register(GdbWaitStoppedTool::new(gdb_state.clone()));
    reg.register(GdbStatusTool::new(gdb_state.clone()));
    reg.register(GdbStopTool::new(gdb_state));
}

/// Build a lightweight [`ToolRegistry`] for direct CLI invocation.
///
/// This registry contains every built-in tool that can run standalone —
/// no agent loop, no model, and no TUI channel required.
///
/// Tools that are excluded and why:
/// - `task` — spawns a sub-agent, requires a model and runtime context.
/// - `context_query` / `context_reduce` — require a model for semantic search.
/// - `ask_question` — requires a TUI channel to display the interactive modal.
///
/// Tools that need channels (`todo_write`, `switch_mode`) or shared state
/// (GDB, context store, knowledge) are given fresh, session-local instances.
/// Side effects (writing files, executing commands) are real — the same as
/// when the agent calls them.
pub fn build_cli_tool_registry(cfg: &Config) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // ── File ─────────────────────────────────────────────────────────────────
    reg.register(ReadFileTool);
    reg.register(ReadImageTool);
    reg.register(ListDirTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register(EditFileTool);
    reg.register(DeleteFileTool);

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool);
    reg.register(SearchCodebaseTool);

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(ReadLintsTool);
    reg.register(UpdateMemoryTool {
        memory_file: cfg.tools.memory.memory_file.clone(),
    });

    // TodoWriteTool and SwitchModeTool need channels; use throwaway senders
    // whose receiving ends are immediately dropped.  Any events they send are
    // discarded — the side effects (writing the todo list to shared state,
    // changing mode) are irrelevant in a single-shot CLI invocation.
    let (event_tx, _event_rx) = mpsc::channel::<ToolEvent>(16);
    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
    reg.register(TodoWriteTool::new(todos, event_tx.clone()));
    let mode_lock = Arc::new(Mutex::new(AgentMode::Agent));
    reg.register(SwitchModeTool::new(mode_lock, event_tx.clone()));

    // LoadSkillTool with empty skill list (no project root available yet;
    // callers who need skills should pass the path via `load_skill` directly).
    reg.register(LoadSkillTool::new(Shared::empty()));

    // ListKnowledge / SearchKnowledge with an empty knowledge store.
    let knowledge = Shared::empty();
    reg.register(ListKnowledgeTool {
        knowledge: knowledge.clone(),
    });
    reg.register(SearchKnowledgeTool {
        knowledge: knowledge.clone(),
    });

    // ── Terminal ──────────────────────────────────────────────────────────────
    reg.register(RunTerminalCommandTool {
        timeout_secs: cfg.tools.timeout_secs,
    });
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
    });

    // ── Context (memory-mapped large-file tools) ──────────────────────────────
    let context_store = Arc::new(Mutex::new(ContextStore::new()));
    reg.register(ContextOpenTool::new(context_store.clone()));
    reg.register(ContextReadTool::new(context_store.clone()));
    reg.register(ContextGrepTool::new(context_store.clone()));

    // ── GDB ───────────────────────────────────────────────────────────────────
    let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
    reg.register(GdbStartServerTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbConnectTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbCommandTool::new(
        gdb_state.clone(),
        cfg.tools.gdb.clone(),
    ));
    reg.register(GdbInterruptTool::new(gdb_state.clone()));
    reg.register(GdbWaitStoppedTool::new(gdb_state.clone()));
    reg.register(GdbStatusTool::new(gdb_state.clone()));
    reg.register(GdbStopTool::new(gdb_state));

    reg
}
