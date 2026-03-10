// Copyright (c) 2024-2026 Martin Schröder <info@swedishembedded.com>
//
// SPDX-License-Identifier: Apache-2.0
//! Centralised tool-registry builder.
//!
//! All callers (CI runner, conversation runner, TUI, sub-agents) use
//! `build_tool_registry` with the appropriate [`ToolSetProfile`] instead of
//! each inlining their own registration loop.
//!
//! ## Tool consolidation
//!
//! The registry now exposes 14-15 high-quality compound tools instead of 42
//! individual tools. This reduces the model's decision surface, cuts input
//! token cost, and keeps the Anthropic prefix-cache stable across turns.
//!
//! Individual tools (`gdb_start_server`, `buf_read`, etc.) are preserved as
//! Rust types for testing and internal use but are no longer registered as
//! separate entries in the tool registry.

use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use sven_config::Config;
use sven_model::ModelProvider;
use sven_runtime::Shared;
use sven_tools::{
    events::{TodoItem, ToolEvent},
    AskQuestionTool, ContextStore, EditFileTool, FindFileTool, GdbSessionState, GrepTool,
    LoadSkillTool, MemoryTool, OutputBufferStore, QuestionRequest, ReadFileTool, ShellTool,
    TodoTool, ToolRegistry, WebFetchTool, WebSearchTool, WriteTool,
};

use sven_core::AgentRuntimeContext;

use crate::context::ToolSetProfile;
use crate::context_tool::ContextTool;
use crate::task_tool::TaskTool;
use crate::GdbTool;

/// Build a [`ToolRegistry`] populated according to the given `profile`.
///
/// This is the single canonical place where tools are wired up.
///
/// ### Shared-state parameters
///
/// * `mode_lock` — unused after removing `SwitchModeTool`; kept for API compatibility.
/// * `tool_event_tx` — the sending half of the channel whose receiving end is
///   passed to `Agent::new()`. `TodoTool` sends events here.
///
/// The `buffer_store` is now bundled inside the `profile` variants that need it
/// (`Full`, `Coding`, `SubAgent`).
pub fn build_tool_registry(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    profile: ToolSetProfile,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    sub_agent_runtime: AgentRuntimeContext,
) -> ToolRegistry {
    match profile {
        ToolSetProfile::Full {
            question_tx,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            question_tx,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: true,
        }),
        ToolSetProfile::Coding {
            question_tx,
            todos,
            buffer_store,
        } => build_profile_full(FullProfileParams {
            cfg,
            model,
            question_tx,
            todos,
            tool_event_tx,
            runtime: &sub_agent_runtime,
            buffer_store,
            include_gdb_context: false,
        }),
        ToolSetProfile::Research { question_tx, todos } => {
            build_profile_research(cfg, question_tx, todos, tool_event_tx, &sub_agent_runtime)
        }
        ToolSetProfile::SubAgent {
            todos,
            buffer_store,
        } => build_profile_subagent(
            cfg,
            model,
            todos,
            tool_event_tx,
            &sub_agent_runtime,
            buffer_store,
        ),
    }
}

/// Parameters shared by the Full and Coding profile builders.
struct FullProfileParams<'a> {
    cfg: &'a Config,
    model: Arc<dyn ModelProvider>,
    question_tx: Option<mpsc::Sender<QuestionRequest>>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &'a AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_gdb_context: bool,
}

/// Full and Coding profiles share the same builder; `include_gdb_context`
/// controls whether GDB and context tools are included.
fn build_profile_full(p: FullProfileParams<'_>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    register_base_tools(
        &mut reg,
        p.cfg,
        p.model,
        p.tool_event_tx.clone(),
        p.runtime,
        Arc::clone(&p.buffer_store),
        p.include_gdb_context,
    );

    if let Some(tx) = p.question_tx {
        reg.register(AskQuestionTool::new_tui(tx));
    }
    reg.register(TodoTool::new(p.todos, p.tool_event_tx.clone()));

    reg.register(TaskTool::new(
        Arc::clone(&p.buffer_store),
        p.tool_event_tx,
        Some(format!("{}/{}", p.cfg.model.provider, p.cfg.model.name)),
    ));

    reg
}

/// Research profile: read-only, no write tools, no task spawning.
fn build_profile_research(
    cfg: &Config,
    question_tx: Option<mpsc::Sender<QuestionRequest>>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // Read-only file tools only.
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(GrepTool);
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        runtime.knowledge.clone(),
    ));
    reg.register(LoadSkillTool::new(runtime.skills.clone()));

    if let Some(tx) = question_tx {
        reg.register(AskQuestionTool::new_tui(tx));
    }
    reg.register(TodoTool::new(todos, tool_event_tx.clone()));

    // Task is included for delegation but limited to research mode.
    let buffer_store = Arc::new(Mutex::new(OutputBufferStore::new()));
    reg.register(TaskTool::new(
        buffer_store,
        tool_event_tx,
        Some(format!("{}/{}", cfg.model.provider, cfg.model.name)),
    ));

    reg
}

/// SubAgent profile: Coding minus ask_question minus task.
fn build_profile_subagent(
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    todos: Arc<Mutex<Vec<TodoItem>>>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    register_base_tools(
        &mut reg,
        cfg,
        model,
        tool_event_tx.clone(),
        runtime,
        buffer_store,
        false, // No GDB/context in sub-agents
    );

    // ask_question omitted: sub-agents run headless.
    // TaskTool omitted: prevent unbounded nesting.
    reg.register(TodoTool::new(todos, tool_event_tx));

    reg
}

/// Register the lean consolidated tool set shared by agent profiles.
///
/// `include_full` controls whether the GDB and context tools are included.
/// SubAgent uses the slimmer set (no GDB, no context) since sub-agents
/// typically perform focused coding/research tasks.
#[allow(clippy::too_many_arguments)]
fn register_base_tools(
    reg: &mut ToolRegistry,
    cfg: &Config,
    model: Arc<dyn ModelProvider>,
    tool_event_tx: mpsc::Sender<ToolEvent>,
    runtime: &AgentRuntimeContext,
    buffer_store: Arc<Mutex<OutputBufferStore>>,
    include_full: bool,
) {
    // ── File I/O ─────────────────────────────────────────────────────────────
    // read_file already handles images (auto-detected by extension).
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register_with_display(EditFileTool);

    // ── Search ────────────────────────────────────────────────────────────────
    // grep now supports whole_project=true (replaces search_codebase).
    reg.register(GrepTool);

    // ── Shell ─────────────────────────────────────────────────────────────────
    // shell covers: run commands, delete files, list dirs, run linters.
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
    });

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── Memory (KV + project knowledge) ──────────────────────────────────────
    // Compound tool: set|get|delete|list|search_knowledge|list_knowledge
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        runtime.knowledge.clone(),
    ));

    // ── Skills ────────────────────────────────────────────────────────────────
    reg.register(LoadSkillTool::new(runtime.skills.clone()));

    // ── Context and GDB (Full profile only) ──────────────────────────────────
    if include_full {
        // Compound context tool: open|read|grep|query|reduce
        let context_store = Arc::new(Mutex::new(ContextStore::new()));
        reg.register(ContextTool::new(
            context_store,
            model,
            cfg,
            Some(tool_event_tx),
        ));

        // Compound GDB tool: start_server|connect|command|interrupt|wait_stopped|status|stop
        let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
        reg.register(GdbTool::new(gdb_state, cfg.tools.gdb.clone()));
    } else {
        // Suppress unused warnings for the buffer_store in SubAgent path.
        let _ = buffer_store;
        let _ = tool_event_tx;
    }
}

/// Build a lightweight [`ToolRegistry`] for direct CLI invocation.
///
/// Contains the same consolidated tool set as the agent, minus tools that
/// require a live model or TUI channel. Intended for `sven tool <name> <args>`
/// direct invocation.
pub fn build_cli_tool_registry(cfg: &Config) -> ToolRegistry {
    let mut reg = ToolRegistry::new();

    // ── File I/O ─────────────────────────────────────────────────────────────
    reg.register(ReadFileTool);
    reg.register(FindFileTool);
    reg.register(WriteTool);
    reg.register_with_display(EditFileTool);

    // ── Search ────────────────────────────────────────────────────────────────
    reg.register(GrepTool);

    // ── Web ───────────────────────────────────────────────────────────────────
    reg.register(WebFetchTool);
    reg.register(WebSearchTool {
        api_key: cfg.tools.web.search.api_key.clone(),
    });

    // ── System ────────────────────────────────────────────────────────────────
    reg.register(ShellTool {
        timeout_secs: cfg.tools.timeout_secs,
    });

    let (event_tx, _event_rx) = mpsc::channel::<ToolEvent>(16);
    let todos = Arc::new(Mutex::new(Vec::<TodoItem>::new()));
    reg.register(TodoTool::new(todos, event_tx.clone()));

    reg.register(LoadSkillTool::new(Shared::empty()));

    // ── Memory ────────────────────────────────────────────────────────────────
    let knowledge = Shared::empty();
    reg.register(MemoryTool::new(
        cfg.tools.memory.memory_file.clone(),
        knowledge,
    ));

    // ── Context (no model available for query/reduce) ─────────────────────────
    // Only open/read/grep are fully usable without a model.
    // The compound context tool is not registered in CLI mode since query/reduce
    // require a live model provider.

    // ── GDB ───────────────────────────────────────────────────────────────────
    let gdb_state = Arc::new(Mutex::new(GdbSessionState::default()));
    reg.register(GdbTool::new(gdb_state, cfg.tools.gdb.clone()));

    reg
}
